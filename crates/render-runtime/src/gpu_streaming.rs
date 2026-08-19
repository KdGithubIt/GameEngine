use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GpuUploadBudget {
    pub(crate) max_bytes: u64,
    pub(crate) max_uploads: u32,
}

impl GpuUploadBudget {
    pub(crate) const fn new(max_bytes: u64, max_uploads: u32) -> Self {
        Self {
            max_bytes,
            max_uploads,
        }
    }

    pub(crate) const fn unlimited() -> Self {
        Self::new(u64::MAX, u32::MAX)
    }
}

impl Default for GpuUploadBudget {
    fn default() -> Self {
        Self::unlimited()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct GpuUploadReport {
    pub(crate) queued_bytes: u64,
    pub(crate) uploaded_bytes: u64,
    pub(crate) queued_uploads: u32,
    pub(crate) uploaded_uploads: u32,
    pub(crate) deferred_uploads: u32,
    pub(crate) cache_hits: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum UploadKind {
    Mesh,
    TextureSrgb,
    TextureLinear,
}

pub(crate) struct GpuUploadFrame {
    budget: GpuUploadBudget,
    report: GpuUploadReport,
    seen: HashSet<(UploadKind, usize)>,
}

impl GpuUploadFrame {
    pub(crate) fn new(budget: GpuUploadBudget) -> Self {
        Self {
            budget,
            report: GpuUploadReport::default(),
            seen: HashSet::new(),
        }
    }

    pub(crate) fn reset(&mut self, budget: GpuUploadBudget) {
        self.budget = budget;
        self.report = GpuUploadReport::default();
        self.seen.clear();
    }

    pub(crate) fn request_mesh(&mut self, identity: usize, bytes: u64) -> bool {
        self.request(UploadKind::Mesh, identity, bytes)
    }

    pub(crate) fn request_texture(&mut self, identity: usize, linear: bool, bytes: u64) -> bool {
        self.request(
            if linear {
                UploadKind::TextureLinear
            } else {
                UploadKind::TextureSrgb
            },
            identity,
            bytes,
        )
    }

    pub(crate) fn note_cache_hit(&mut self) {
        self.report.cache_hits = self.report.cache_hits.saturating_add(1);
    }

    pub(crate) const fn report(&self) -> GpuUploadReport {
        self.report
    }

    fn request(&mut self, kind: UploadKind, identity: usize, bytes: u64) -> bool {
        if !self.seen.insert((kind, identity)) {
            return false;
        }
        self.report.queued_uploads = self.report.queued_uploads.saturating_add(1);
        self.report.queued_bytes = self.report.queued_bytes.saturating_add(bytes);
        let count_available = self.report.uploaded_uploads < self.budget.max_uploads;
        let bytes_available =
            self.report.uploaded_bytes.saturating_add(bytes) <= self.budget.max_bytes;
        let first_oversized_upload =
            self.report.uploaded_uploads == 0 && self.budget.max_uploads > 0;
        if count_available && (bytes_available || first_oversized_upload) {
            self.report.uploaded_uploads = self.report.uploaded_uploads.saturating_add(1);
            self.report.uploaded_bytes = self.report.uploaded_bytes.saturating_add(bytes);
            true
        } else {
            self.report.deferred_uploads = self.report.deferred_uploads.saturating_add(1);
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_budget_defers_excess_work_and_tracks_bytes() {
        let mut frame = GpuUploadFrame::new(GpuUploadBudget::new(128, 2));
        assert!(frame.request_mesh(1, 64));
        assert!(frame.request_texture(2, false, 64));
        assert!(!frame.request_texture(3, true, 32));
        assert_eq!(
            frame.report(),
            GpuUploadReport {
                queued_bytes: 160,
                uploaded_bytes: 128,
                queued_uploads: 3,
                uploaded_uploads: 2,
                deferred_uploads: 1,
                cache_hits: 0,
            }
        );
    }

    #[test]
    fn one_oversized_upload_can_make_forward_progress() {
        let mut frame = GpuUploadFrame::new(GpuUploadBudget::new(16, 1));
        assert!(frame.request_mesh(7, 128));
        assert!(!frame.request_mesh(8, 1));
        assert_eq!(frame.report().uploaded_uploads, 1);
        assert_eq!(frame.report().deferred_uploads, 1);
    }
}
