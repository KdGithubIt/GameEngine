//! Content-addressed cache for derived, rebuildable asset bytes.

use std::io;
use std::path::{Path, PathBuf};

/// Content-addressed key for one cache entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey(pub u64);

impl CacheKey {
    /// Renders this key as a fixed-width lowercase hex file stem.
    pub fn file_stem(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// A content-addressed, on-disk cache of disposable derived asset bytes.
#[derive(Debug, Clone)]
pub struct DerivedCache {
    root: PathBuf,
}

impl DerivedCache {
    /// Opens the derived cache rooted at `<project_root>/.engine/cache/`.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            root: project_root.into().join(".engine").join("cache"),
        }
    }

    /// Reads a cache entry, returning `None` on any cache miss.
    pub fn get(&self, domain: &str, key: &CacheKey, extension: &str) -> Option<Vec<u8>> {
        std::fs::read(self.entry_path(domain, key, extension)).ok()
    }

    /// Writes a cache entry, creating the domain directory when necessary.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory or entry cannot be written.
    pub fn put(
        &self,
        domain: &str,
        key: &CacheKey,
        extension: &str,
        bytes: &[u8],
    ) -> io::Result<()> {
        let dir = self.root.join(domain);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(self.entry_path(domain, key, extension), bytes)
    }

    /// Deletes the entire cache root, if present.
    ///
    /// # Errors
    ///
    /// Returns an I/O error for failures other than an absent cache root.
    pub fn clear(&self) -> io::Result<()> {
        match std::fs::remove_dir_all(&self.root) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn entry_path(&self, domain: &str, key: &CacheKey, extension: &str) -> PathBuf {
        self.root
            .join(domain)
            .join(format!("{}.{extension}", key.file_stem()))
    }

    /// Returns the cache root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_then_get_round_trips_bytes() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = DerivedCache::new(dir.path());
        let key = CacheKey(0xdead_beef);
        cache.put("anim", &key, "clip.json", b"hello").expect("put");
        assert_eq!(cache.get("anim", &key, "clip.json").as_deref(), Some(b"hello".as_slice()));
    }

    #[test]
    fn clear_forces_a_cache_miss() {
        let dir = tempfile::tempdir().expect("temp dir");
        let cache = DerivedCache::new(dir.path());
        let key = CacheKey(42);
        cache.put("anim", &key, "clip.json", b"payload").expect("put");
        cache.clear().expect("clear");
        assert!(cache.get("anim", &key, "clip.json").is_none());
    }
}
