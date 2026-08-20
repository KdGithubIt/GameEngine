use super::{CurveAction, clamp_key_tick, interpolation_label, snap_local_tick};
use eframe::egui;
use engine_authoring::{
    DisplayFrameRate, TimelineClip, TimelineInterpolation, TimelineKey, TimelineTick,
};

fn draft_id(clip: &TimelineClip) -> egui::Id {
    egui::Id::new(("sequencer_curve_draft", clip.id.as_str()))
}

pub(super) fn draft_keys(
    ui: &mut egui::Ui,
    clip: &TimelineClip,
    committed: &[TimelineKey],
) -> Vec<TimelineKey> {
    ui.data_mut(|data| data.get_temp::<Vec<TimelineKey>>(draft_id(clip)))
        .unwrap_or_else(|| committed.to_vec())
}

pub(super) fn clear_draft(ui: &mut egui::Ui, clip: &TimelineClip) {
    ui.data_mut(|data| data.remove::<Vec<TimelineKey>>(draft_id(clip)));
}

pub(super) fn show(
    ui: &mut egui::Ui,
    clip: &TimelineClip,
    committed: &[TimelineKey],
    frame_rate: DisplayFrameRate,
    snap_to_frames: bool,
) -> Option<CurveAction> {
    let mut draft = draft_keys(ui, clip, committed);
    let mut changed = false;
    let mut commit = false;
    let mut delete = None;

    egui::Grid::new(("sequencer_curve_keys", clip.id.as_str()))
        .num_columns(5)
        .spacing(egui::vec2(8.0, 4.0))
        .show(ui, |ui| {
            ui.small("Key");
            ui.small("Time");
            ui.small("Value");
            ui.small("Interpolation");
            ui.small("");
            ui.end_row();

            for index in 0..draft.len() {
                ui.label(index.to_string());

                let mut seconds = draft[index].tick.as_seconds();
                let time = ui.add(
                    egui::DragValue::new(&mut seconds)
                        .speed(0.01)
                        .range(0.0..=clip.duration().as_seconds())
                        .suffix(" s"),
                );
                if time.changed() {
                    let mut tick = TimelineTick::from_seconds(seconds);
                    if snap_to_frames {
                        tick = snap_local_tick(
                            frame_rate,
                            clip.start,
                            clip.duration(),
                            tick,
                        );
                    }
                    let clamped = clamp_key_tick(&draft, index, tick, clip.duration());
                    draft[index].tick = clamped;
                    changed = true;
                }
                commit |= time.drag_stopped() || time.lost_focus();

                let value = ui.add(egui::DragValue::new(&mut draft[index].value).speed(0.05));
                changed |= value.changed();
                commit |= value.drag_stopped() || value.lost_focus();

                let previous = draft[index].interpolation;
                egui::ComboBox::from_id_salt((
                    "sequencer_curve_interpolation",
                    clip.id.as_str(),
                    index,
                ))
                .selected_text(interpolation_label(previous))
                .show_ui(ui, |ui| {
                    for mode in [
                        TimelineInterpolation::Step,
                        TimelineInterpolation::Linear,
                        TimelineInterpolation::Smooth,
                    ] {
                        ui.selectable_value(
                            &mut draft[index].interpolation,
                            mode,
                            interpolation_label(mode),
                        );
                    }
                });
                if draft[index].interpolation != previous {
                    changed = true;
                    commit = true;
                }

                if ui
                    .add_enabled(draft.len() > 1, egui::Button::new("Delete"))
                    .clicked()
                {
                    delete = Some(index);
                }
                ui.end_row();
            }
        });

    if let Some(index) = delete {
        clear_draft(ui, clip);
        return Some(CurveAction::DeleteKey(index));
    }
    if changed {
        ui.data_mut(|data| data.insert_temp(draft_id(clip), draft.clone()));
    }
    if commit && draft != committed {
        clear_draft(ui, clip);
        return Some(CurveAction::ReplaceKeys(draft));
    }
    None
}
