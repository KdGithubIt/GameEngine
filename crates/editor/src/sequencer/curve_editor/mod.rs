//! Numeric curve editing for the Sequencer Property track.
//!
//! This module edits the existing persisted Property payload. It does not
//! introduce an Editor-only curve format or persistence path.

use super::LoadedTimeline;
use eframe::egui;
use engine_authoring::{
    AuthoringPermissions, DisplayFrameRate, TimelineClip, TimelineClipPayload,
    TimelineInterpolation, TimelineKey, TimelineProperty, TimelineTick,
};

mod graph;
mod rows;

const PROPERTY_CHOICES: [(TimelineProperty, &str); 9] = [
    (TimelineProperty::TranslationX, "Translation X"),
    (TimelineProperty::TranslationY, "Translation Y"),
    (TimelineProperty::TranslationZ, "Translation Z"),
    (TimelineProperty::RotationX, "Rotation X"),
    (TimelineProperty::RotationY, "Rotation Y"),
    (TimelineProperty::RotationZ, "Rotation Z"),
    (TimelineProperty::ScaleX, "Scale X"),
    (TimelineProperty::ScaleY, "Scale Y"),
    (TimelineProperty::ScaleZ, "Scale Z"),
];

#[derive(Debug, Clone)]
pub(super) enum CurveAction {
    SetProperty(TimelineProperty),
    AddKey(TimelineKey),
    ReplaceKeys(Vec<TimelineKey>),
    DeleteKey(usize),
}

impl CurveAction {
    const fn success_message(&self) -> &'static str {
        match self {
            Self::SetProperty(_) => "Changed the animated property",
            Self::AddKey(_) => "Added a curve key",
            Self::ReplaceKeys(_) => "Edited the curve",
            Self::DeleteKey(_) => "Deleted a curve key",
        }
    }
}

pub(super) fn show(
    ui: &mut egui::Ui,
    loaded: &mut LoadedTimeline,
    track_index: usize,
    clip_index: usize,
    permissions: &AuthoringPermissions,
    snap_to_frames: bool,
    status: &mut Option<String>,
) {
    let Some(clip) = loaded
        .document
        .tracks
        .get(track_index)
        .and_then(|track| track.clips.get(clip_index))
        .cloned()
    else {
        return;
    };
    let TimelineClipPayload::Property { property, keys } = &clip.payload else {
        return;
    };

    ui.separator();
    ui.strong("Curve Editor");
    ui.small("Transform channels use the persisted Property-track numeric curve.");

    let mut action = None;
    let mut selected_property = *property;
    ui.horizontal_wrapped(|ui| {
        ui.label("Property");
        egui::ComboBox::from_id_salt(("sequencer_curve_property", clip.id.as_str()))
            .selected_text(property_label(selected_property))
            .show_ui(ui, |ui| {
                for (choice, label) in PROPERTY_CHOICES {
                    ui.selectable_value(&mut selected_property, choice, label);
                }
            });
        if selected_property != *property {
            action = Some(CurveAction::SetProperty(selected_property));
        }

        if ui.button("+ Key at playhead").clicked() {
            match key_at_playhead(
                &clip,
                keys,
                loaded.preview_tick,
                loaded.document.display_frame_rate,
                snap_to_frames,
            ) {
                Ok(key) if action.is_none() => action = Some(CurveAction::AddKey(key)),
                Ok(_) => {}
                Err(error) => *status = Some(error),
            }
        }
    });

    let draft = rows::draft_keys(ui, &clip, keys);
    graph::show(ui, &clip, &draft, loaded.preview_tick);

    if action.is_none() {
        action = rows::show(
            ui,
            &clip,
            keys,
            loaded.document.display_frame_rate,
            snap_to_frames,
        );
    }

    if let Some(action) = action {
        let message = action.success_message();
        let result = loaded.edit(permissions, |document| {
            let Some(clip) = document
                .tracks
                .get_mut(track_index)
                .and_then(|track| track.clips.get_mut(clip_index))
            else {
                return;
            };
            let TimelineClipPayload::Property { property, keys } = &mut clip.payload else {
                return;
            };
            match action {
                CurveAction::SetProperty(next) => *property = next,
                CurveAction::AddKey(key) => {
                    keys.push(key);
                    keys.sort_by_key(|key| key.tick);
                }
                CurveAction::ReplaceKeys(replacement) => *keys = replacement,
                CurveAction::DeleteKey(index) => {
                    if keys.len() > 1 && index < keys.len() {
                        keys.remove(index);
                    }
                }
            }
        });
        rows::clear_draft(ui, &clip);
        *status = Some(match result {
            Ok(()) => message.to_owned(),
            Err(error) => format!("Curve edit rejected: {error}"),
        });
    }
}

pub(super) fn property_label(property: TimelineProperty) -> &'static str {
    PROPERTY_CHOICES
        .iter()
        .find_map(|(candidate, label)| (*candidate == property).then_some(*label))
        .unwrap_or("Property")
}

pub(super) const fn interpolation_label(interpolation: TimelineInterpolation) -> &'static str {
    match interpolation {
        TimelineInterpolation::Step => "Step",
        TimelineInterpolation::Linear => "Linear",
        TimelineInterpolation::Smooth => "Smooth",
    }
}

pub(super) fn snap_local_tick(
    frame_rate: DisplayFrameRate,
    clip_start: TimelineTick,
    duration: TimelineTick,
    local_tick: TimelineTick,
) -> TimelineTick {
    let absolute = TimelineTick(clip_start.get().saturating_add(local_tick.get()));
    let snapped = frame_rate.snap(absolute).unwrap_or(absolute);
    TimelineTick((snapped.get() - clip_start.get()).clamp(0, duration.get()))
}

pub(super) fn clamp_key_tick(
    keys: &[TimelineKey],
    index: usize,
    candidate: TimelineTick,
    duration: TimelineTick,
) -> TimelineTick {
    let minimum = index
        .checked_sub(1)
        .and_then(|previous| keys.get(previous))
        .map_or(0, |key| key.tick.get().saturating_add(1));
    let maximum = keys
        .get(index + 1)
        .map_or(duration.get(), |key| key.tick.get().saturating_sub(1));
    TimelineTick(candidate.get().clamp(minimum, maximum.max(minimum)))
}

pub(super) fn sample_keys(keys: &[TimelineKey], tick: TimelineTick) -> f32 {
    let Some(first) = keys.first().copied() else {
        return 0.0;
    };
    if tick <= first.tick {
        return first.value;
    }
    let last = keys.last().copied().unwrap_or(first);
    if tick >= last.tick {
        return last.value;
    }
    for segment in keys.windows(2) {
        let (left, right) = (segment[0], segment[1]);
        if tick < left.tick || tick >= right.tick {
            continue;
        }
        let span = (right.tick.get() - left.tick.get()).max(1);
        let progress = (tick.get() - left.tick.get()) as f32 / span as f32;
        return match left.interpolation {
            TimelineInterpolation::Step => left.value,
            TimelineInterpolation::Linear => left.value + (right.value - left.value) * progress,
            TimelineInterpolation::Smooth => {
                let eased = progress * progress * (3.0 - 2.0 * progress);
                left.value + (right.value - left.value) * eased
            }
        };
    }
    last.value
}

fn key_at_playhead(
    clip: &TimelineClip,
    keys: &[TimelineKey],
    preview_tick: TimelineTick,
    frame_rate: DisplayFrameRate,
    snap_to_frames: bool,
) -> Result<TimelineKey, String> {
    let mut tick = TimelineTick(
        (preview_tick.get() - clip.start.get()).clamp(0, clip.duration().get()),
    );
    if snap_to_frames {
        tick = snap_local_tick(frame_rate, clip.start, clip.duration(), tick);
    }
    if keys.iter().any(|key| key.tick == tick) {
        return Err(format!("A curve key already exists at {:.3}s", tick.as_seconds()));
    }
    Ok(TimelineKey {
        tick,
        value: sample_keys(keys, tick),
        interpolation: keys
            .iter()
            .rev()
            .find(|key| key.tick < tick)
            .map_or(TimelineInterpolation::Linear, |key| key.interpolation),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(tick: i64, value: f32, interpolation: TimelineInterpolation) -> TimelineKey {
        TimelineKey {
            tick: TimelineTick(tick),
            value,
            interpolation,
        }
    }

    #[test]
    fn displayed_sampling_matches_runtime_interpolation_contract() {
        let linear = [
            key(0, 0.0, TimelineInterpolation::Linear),
            key(100, 10.0, TimelineInterpolation::Linear),
        ];
        assert_eq!(sample_keys(&linear, TimelineTick(50)), 5.0);

        let step = [
            key(0, 2.0, TimelineInterpolation::Step),
            key(100, 8.0, TimelineInterpolation::Linear),
        ];
        assert_eq!(sample_keys(&step, TimelineTick(99)), 2.0);

        let smooth = [
            key(0, 0.0, TimelineInterpolation::Smooth),
            key(100, 10.0, TimelineInterpolation::Linear),
        ];
        assert_eq!(sample_keys(&smooth, TimelineTick(50)), 5.0);
    }

    #[test]
    fn key_time_cannot_cross_neighbor_keys() {
        let keys = [
            key(10, 0.0, TimelineInterpolation::Linear),
            key(20, 1.0, TimelineInterpolation::Linear),
            key(30, 2.0, TimelineInterpolation::Linear),
        ];
        assert_eq!(
            clamp_key_tick(&keys, 1, TimelineTick(0), TimelineTick(40)),
            TimelineTick(11)
        );
        assert_eq!(
            clamp_key_tick(&keys, 1, TimelineTick(40), TimelineTick(40)),
            TimelineTick(29)
        );
    }

    #[test]
    fn frame_snapping_uses_absolute_timeline_time() {
        let rate = DisplayFrameRate {
            numerator: 60,
            denominator: 1,
        };
        assert_eq!(
            snap_local_tick(
                rate,
                TimelineTick(400),
                TimelineTick(2_000),
                TimelineTick(350)
            ),
            TimelineTick(400)
        );
    }
}
