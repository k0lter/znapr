use gilt::console::Console;
use gilt::progress::{Progress, TaskId};

use crate::logger::progress_enabled;

/// A thin, always-safe wrapper around gilt's progress display.
///
/// When progress is disabled (no tty, verbose logging, or empty work set) every
/// method is a no-op, so callers never need to branch.
pub struct Bar {
    inner: Option<(Progress, TaskId)>,
}

impl Bar {
    pub fn new(description: &str, total: usize) -> Self {
        if total == 0 || !progress_enabled() {
            return Bar { inner: None };
        }

        let mut progress = Progress::new(Progress::default_columns())
            .with_console(Console::builder().markup(true).build())
            // Transient: the bar erases itself on stop, leaving clean output.
            .with_transient(true);
        let task = progress.add_task(description, Some(total as f64), true);
        progress.start();

        Bar {
            inner: Some((progress, task)),
        }
    }

    pub fn set_message(&mut self, message: &str) {
        if let Some((p, task)) = &mut self.inner {
            p.update(*task, None, None, None, Some(message), None, None);
        }
    }

    pub fn inc(&mut self) {
        if let Some((p, task)) = &mut self.inner {
            p.advance(*task, 1.0);
        }
    }

    pub fn finish(&mut self) {
        if let Some((mut p, _)) = self.inner.take() {
            p.stop();
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Progress display for `backup`: an outer bar over configuration files plus a
/// transient inner bar that tracks the current rsync transfer.
///
/// Like [`Bar`], every method is a no-op when progress is disabled (no tty,
/// verbose logging, or empty work set).
pub struct BackupProgress {
    inner: Option<(Progress, TaskId)>,
}

impl BackupProgress {
    pub fn new(total: usize) -> Self {
        if total == 0 || !progress_enabled() {
            return BackupProgress { inner: None };
        }

        // Manual refresh (no background thread): the rsync loop refreshes on
        // every parsed percentage, which is frequent enough to feel live.
        let mut progress = Progress::new(Progress::default_columns())
            .with_console(Console::builder().markup(true).build())
            .with_auto_refresh(false)
            .with_transient(true);
        let outer = progress.add_task("backups", Some(total as f64), true);
        progress.start();

        BackupProgress {
            inner: Some((progress, outer)),
        }
    }

    pub fn set_outer(&mut self, message: &str) {
        if let Some((p, outer)) = &mut self.inner {
            p.update(*outer, None, None, None, Some(message), None, None);
            p.refresh();
        }
    }

    pub fn advance_outer(&mut self) {
        if let Some((p, outer)) = &mut self.inner {
            p.advance(*outer, 1.0);
            p.refresh();
        }
    }

    /// Add a transient rsync task (0-100%). Returns its id, or `None` when the
    /// display is disabled.
    pub fn start_rsync(&mut self, description: &str) -> Option<TaskId> {
        let (p, _) = self.inner.as_mut()?;
        let id = p.add_task(description, Some(100.0), true);
        p.refresh();
        Some(id)
    }

    pub fn set_rsync(&mut self, id: TaskId, percent: u8) {
        if let Some((p, _)) = &mut self.inner {
            p.update(id, Some(percent as f64), None, None, None, None, None);
            p.refresh();
        }
    }

    pub fn end_rsync(&mut self, id: TaskId) {
        if let Some((p, _)) = &mut self.inner {
            p.remove_task(id);
            p.refresh();
        }
    }

    pub fn finish(&mut self) {
        if let Some((mut p, _)) = self.inner.take() {
            p.stop();
        }
    }
}

impl Drop for BackupProgress {
    fn drop(&mut self) {
        self.finish();
    }
}
