//! Debounced project filesystem observation without mutating authoring data.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime};

/// Project area affected by a normalized filesystem event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSyncArea {
    /// Physical file or folder below `assets/`.
    Assets,
    /// Authored or generated Rust source below `game/src/`.
    GameSource,
    /// Root `asset_manifest.json`.
    Manifest,
}

/// Change kind detected between two stable polling snapshots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileSyncKind {
    /// A path appeared.
    Created,
    /// A path retained identity but content metadata changed.
    Modified,
    /// A path disappeared.
    Removed,
}

/// One project-relative, normalized synchronization event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSyncEvent {
    /// Project-relative path using the host path representation.
    pub relative_path: PathBuf,
    /// Owned project area.
    pub area: FileSyncArea,
    /// Observed change.
    pub kind: FileSyncKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    length: u64,
    is_directory: bool,
}

enum WatcherCommand {
    Suppress(PathBuf),
    Stop,
}

enum WatcherMessage {
    Ready,
    Events(Vec<FileSyncEvent>),
}

/// Background polling watcher with explicit internal-write suppression.
pub struct ProjectFileWatcher {
    command_sender: mpsc::Sender<WatcherCommand>,
    message_receiver: mpsc::Receiver<WatcherMessage>,
    next_poll: Instant,
    interval: Duration,
    ready: bool,
}

impl std::fmt::Debug for ProjectFileWatcher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProjectFileWatcher")
            .field("next_poll", &self.next_poll)
            .field("interval", &self.interval)
            .field("ready", &self.ready)
            .finish_non_exhaustive()
    }
}

impl ProjectFileWatcher {
    /// Captures the initial project snapshot.
    pub fn new(project_root: PathBuf) -> Self {
        let interval = Duration::from_millis(500);
        let (command_sender, command_receiver) = mpsc::channel();
        let (message_sender, message_receiver) = mpsc::channel();
        let _ = std::thread::Builder::new()
            .name("project-file-watcher".to_owned())
            .spawn(move || watcher_worker(project_root, interval, command_receiver, message_sender));
        Self {
            command_sender,
            message_receiver,
            next_poll: Instant::now() + interval,
            interval,
            ready: false,
        }
    }

    /// Marks one project-relative internal write so its next event is consumed.
    pub fn suppress_once(&mut self, relative_path: impl Into<PathBuf>) {
        let _ = self
            .command_sender
            .send(WatcherCommand::Suppress(relative_path.into()));
    }

    /// Returns debounced events when the next polling deadline is reached.
    pub fn poll(&mut self) -> Vec<FileSyncEvent> {
        let mut events = Vec::new();
        while let Ok(message) = self.message_receiver.try_recv() {
            match message {
                WatcherMessage::Ready => self.ready = true,
                WatcherMessage::Events(mut received) => events.append(&mut received),
            }
        }
        self.next_poll = Instant::now() + self.interval;
        events
    }

    /// Duration until the next polling deadline.
    pub fn time_until_poll(&self) -> Duration {
        self.next_poll.saturating_duration_since(Instant::now())
    }

    #[cfg(test)]
    fn is_ready(&self) -> bool {
        self.ready
    }
}

impl Drop for ProjectFileWatcher {
    fn drop(&mut self) {
        let _ = self.command_sender.send(WatcherCommand::Stop);
    }
}

fn watcher_worker(
    project_root: PathBuf,
    interval: Duration,
    command_receiver: mpsc::Receiver<WatcherCommand>,
    message_sender: mpsc::Sender<WatcherMessage>,
) {
    let mut snapshot = capture_snapshot(&project_root);
    let mut suppressed = BTreeSet::new();
    if message_sender.send(WatcherMessage::Ready).is_err() {
        return;
    }
    loop {
        match command_receiver.recv_timeout(interval) {
            Ok(WatcherCommand::Suppress(path)) => {
                suppressed.insert(path);
                while let Ok(command) = command_receiver.try_recv() {
                    match command {
                        WatcherCommand::Suppress(path) => {
                            suppressed.insert(path);
                        }
                        WatcherCommand::Stop => return,
                    }
                }
            }
            Ok(WatcherCommand::Stop) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let next = capture_snapshot(&project_root);
                let events = diff_snapshots(&snapshot, &next, &mut suppressed);
                snapshot = next;
                if !events.is_empty()
                    && message_sender.send(WatcherMessage::Events(events)).is_err()
                {
                    return;
                }
            }
        }
    }
}

fn diff_snapshots(
    previous: &BTreeMap<PathBuf, FileStamp>,
    next: &BTreeMap<PathBuf, FileStamp>,
    suppressed: &mut BTreeSet<PathBuf>,
) -> Vec<FileSyncEvent> {
    let mut paths = previous
        .keys()
        .chain(next.keys())
        .cloned()
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let mut events = Vec::new();
    for path in paths {
        let kind = match (previous.get(&path), next.get(&path)) {
            (None, Some(_)) => Some(FileSyncKind::Created),
            (Some(_), None) => Some(FileSyncKind::Removed),
            (Some(before), Some(after)) if before != after => Some(FileSyncKind::Modified),
            _ => None,
        };
        let Some(kind) = kind else {
            continue;
        };
        if suppressed.remove(&path) {
            continue;
        }
        if let Some(area) = classify_area(&path) {
            events.push(FileSyncEvent {
                relative_path: path,
                area,
                kind,
            });
        }
    }
    events
}

fn capture_snapshot(project_root: &Path) -> BTreeMap<PathBuf, FileStamp> {
    let mut snapshot = BTreeMap::new();
    scan_root(project_root, &project_root.join("assets"), &mut snapshot);
    let manifest = project_root.join("asset_manifest.json");
    if let Ok(metadata) = std::fs::metadata(&manifest) {
        snapshot.insert(PathBuf::from("asset_manifest.json"), stamp(&metadata));
    }
    snapshot
}

fn scan_root(project_root: &Path, directory: &Path, snapshot: &mut BTreeMap<PathBuf, FileStamp>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        if let Ok(relative) = path.strip_prefix(project_root) {
            snapshot.insert(relative.to_path_buf(), stamp(&metadata));
        }
        if file_type.is_dir() {
            scan_root(project_root, &path, snapshot);
        }
    }
}

fn stamp(metadata: &std::fs::Metadata) -> FileStamp {
    FileStamp {
        modified: metadata.modified().ok(),
        length: metadata.len(),
        is_directory: metadata.is_dir(),
    }
}

fn classify_area(path: &Path) -> Option<FileSyncArea> {
    if path == Path::new("asset_manifest.json") {
        Some(FileSyncArea::Manifest)
    } else if path.starts_with("assets") {
        Some(FileSyncArea::Assets)
    } else if path.starts_with(Path::new("game").join("src")) {
        Some(FileSyncArea::GameSource)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn watcher_reports_normalized_asset_and_source_changes() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("assets/ui")).unwrap();
        std::fs::create_dir_all(directory.path().join("assets/scripts/rust/components")).unwrap();
        let mut watcher = ProjectFileWatcher::new(directory.path().to_path_buf());
        wait_until_ready(&mut watcher);
        std::fs::write(directory.path().join("assets/ui/hud.ui.json"), "{}").unwrap();
        std::fs::write(
            directory
                .path()
                .join("assets/scripts/rust/components/health.rs"),
            "",
        )
        .unwrap();
        let events = wait_for_events(&mut watcher);
        assert!(events
            .iter()
            .any(|event| event.area == FileSyncArea::Assets));
        assert!(events.iter().any(|event| {
            event.area == FileSyncArea::Assets
                && event
                    .relative_path
                    .starts_with("assets/scripts/rust/components")
        }));
    }

    #[test]
    fn suppression_consumes_one_internal_write_event() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(directory.path().join("assets")).unwrap();
        let mut watcher = ProjectFileWatcher::new(directory.path().to_path_buf());
        wait_until_ready(&mut watcher);
        watcher.suppress_once(PathBuf::from("assets/internal.txt"));
        std::fs::write(directory.path().join("assets/internal.txt"), "internal").unwrap();
        std::thread::sleep(Duration::from_millis(650));
        assert!(watcher.poll().is_empty());
    }

    fn wait_until_ready(watcher: &mut ProjectFileWatcher) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !watcher.is_ready() {
            let _ = watcher.poll();
            assert!(Instant::now() < deadline, "watcher initialization timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn wait_for_events(watcher: &mut ProjectFileWatcher) -> Vec<FileSyncEvent> {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let events = watcher.poll();
            if !events.is_empty() {
                return events;
            }
            assert!(Instant::now() < deadline, "watcher event timed out");
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
