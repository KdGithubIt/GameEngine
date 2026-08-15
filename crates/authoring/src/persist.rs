//! Shared persistence helpers for authoring adapters.
//!
//! Adapters own when to save files, but the low-level replacement behavior is
//! shared so CLI, MCP, and editor code do not duplicate platform-specific I/O.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// File persistence operation that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersistOperation {
    /// Creating the replacement file failed.
    CreateReplacement,
    /// Writing or syncing the replacement file failed.
    WriteReplacement,
    /// Replacing the target file failed.
    ReplaceTarget,
}

/// Error returned by shared persistence helpers.
#[derive(Debug)]
pub struct PersistError {
    operation: PersistOperation,
    path: PathBuf,
    detail_path: Option<PathBuf>,
    source: io::Error,
}

impl PersistError {
    /// Creates a persistence error.
    pub fn new(operation: PersistOperation, path: PathBuf, source: io::Error) -> Self {
        Self {
            operation,
            path,
            detail_path: None,
            source,
        }
    }

    /// Adds an operation-specific detail path, such as a replacement file path.
    pub fn with_detail_path(mut self, detail_path: PathBuf) -> Self {
        self.detail_path = Some(detail_path);
        self
    }

    /// Returns the operation that failed.
    pub fn operation(&self) -> PersistOperation {
        self.operation
    }

    /// Returns the target path requested by the caller.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns an operation-specific detail path, when one exists.
    pub fn detail_path(&self) -> Option<&Path> {
        self.detail_path.as_deref()
    }

    /// Returns an AI-facing message without duplicating the structured path.
    pub fn message(&self) -> String {
        format!("{:?} failed: {}", self.operation, self.source)
    }
}

impl fmt::Display for PersistError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:?} failed for `{}`",
            self.operation,
            self.path.display()
        )?;
        if let Some(detail_path) = &self.detail_path {
            write!(formatter, " while using `{}`", detail_path.display())?;
        }
        write!(formatter, ": {}", self.source)
    }
}

impl std::error::Error for PersistError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.source)
    }
}

/// Replaces `path` with `contents` without writing directly to the target file.
///
/// When the target already contains the exact requested bytes, it is left
/// untouched so callers do not create false file-change or rebuild signals.
/// Otherwise, the replacement is written to a sibling temporary file first,
/// synced, and then moved over the target path.
///
/// # Errors
///
/// Returns a [`PersistError`] for replacement file creation, write/sync, or
/// target replacement failures. The structured error path identifies the target
/// file requested by the caller.
pub fn replace_file_contents(path: &Path, contents: &str) -> Result<(), PersistError> {
    if fs::read(path).is_ok_and(|existing| existing == contents.as_bytes()) {
        return Ok(());
    }

    let (temp_path, mut file) = create_replacement_file(path).map_err(|source| {
        PersistError::new(PersistOperation::CreateReplacement, path.to_owned(), source)
    })?;

    let write_result = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);

    if let Err(source) = write_result {
        let _ = fs::remove_file(&temp_path);
        return Err(
            PersistError::new(PersistOperation::WriteReplacement, path.to_owned(), source)
                .with_detail_path(temp_path),
        );
    }

    if let Err(source) = replace_file(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(PersistError::new(
            PersistOperation::ReplaceTarget,
            path.to_owned(),
            source,
        ));
    }

    Ok(())
}

fn create_replacement_file(path: &Path) -> io::Result<(PathBuf, fs::File)> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_else(|| "graph.json".into());

    for attempt in 0..1000_u32 {
        let temp_path = parent.join(format!(
            ".{file_name}.{}.{}.tmp",
            std::process::id(),
            attempt
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)
        {
            Ok(file) => return Ok((temp_path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique replacement file",
    ))
}

#[cfg(not(windows))]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let flags = MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH;

    // SAFETY: both buffers are null-terminated and live for the duration of
    // the call; the function does not retain the pointers after it returns.
    let replaced = unsafe { MoveFileExW(source_wide.as_ptr(), target_wide.as_ptr(), flags) };
    if replaced == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    #[test]
    fn identical_contents_leave_target_untouched() {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should be after the Unix epoch")
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "engine-authoring-persist-{}-{unique}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).expect("temporary directory should be created");
        let target = directory.join("project_modules.rs");
        fs::write(&target, "same contents\n").expect("target should be written");
        let before = fs::metadata(&target)
            .and_then(|metadata| metadata.modified())
            .expect("target modification time should be available");

        std::thread::sleep(Duration::from_millis(20));
        replace_file_contents(&target, "same contents\n")
            .expect("identical contents should not require replacement");

        let after = fs::metadata(&target)
            .and_then(|metadata| metadata.modified())
            .expect("target modification time should remain available");
        assert_eq!(after, before);
        fs::remove_dir_all(directory).expect("temporary directory should be removed");
    }

    #[test]
    fn write_replacement_error_reports_target_path_as_primary_path() {
        let target = PathBuf::from("graph.json");
        let temp = PathBuf::from(".graph.json.123.0.tmp");
        let error = PersistError::new(
            PersistOperation::WriteReplacement,
            target.clone(),
            io::Error::new(io::ErrorKind::WriteZero, "write failed"),
        )
        .with_detail_path(temp.clone());

        assert_eq!(error.path(), target.as_path());
        assert_eq!(error.detail_path(), Some(temp.as_path()));
        assert!(!error.message().contains("graph.json"));
    }
}
