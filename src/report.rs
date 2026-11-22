use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use chrono::{DateTime, Local};
use minijinja::Environment;
use serde::Serialize;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Failed,
}

#[derive(Clone)]
pub struct VolumeReport {
    pub name: String,
    pub source: String,
    pub status: Status,
    pub error: Option<String>,
    pub duration: Duration,
    /// rsync 24 accepted with a warning (`ignore`, or the last `retry`).
    /// `silent` stays false: the report must not mention that 24.
    pub vanished: bool,
}

#[derive(Clone)]
pub struct BackupReport {
    pub config_file: String,
    /// YAML `name`. Absent when the file did not load.
    pub profile: Option<String>,
    /// Remote host. Absent when the file did not load. A row is this host.
    pub host: Option<String>,
    /// Config-level failure (e.g. the YAML could not be loaded/validated).
    pub error: Option<String>,
    pub volumes: Vec<VolumeReport>,
    pub duration: Duration,
}

#[derive(Clone)]
pub struct RunReport {
    pub hostname: String,
    pub started: DateTime<Local>,
    pub duration: Duration,
    pub backups: Vec<BackupReport>,
}

impl RunReport {
    pub fn has_errors(&self) -> bool {
        self.backups
            .iter()
            .any(|b| b.error.is_some() || b.volumes.iter().any(|v| v.status == Status::Failed))
    }

    pub fn counts(&self) -> (usize, usize) {
        let (mut ok, mut failed) = (0, 0);
        for b in &self.backups {
            for v in &b.volumes {
                match v.status {
                    Status::Ok => ok += 1,
                    Status::Failed => failed += 1,
                }
            }
        }
        (ok, failed)
    }
}

pub fn hostname() -> String {
    // Linux-specific; falls back gracefully. Backups run on the host itself.
    std::fs::read_to_string("/etc/hostname")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

fn fmt_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let (days, hours, mins, s) = (
        secs / 86_400,
        (secs % 86_400) / 3_600,
        (secs % 3_600) / 60,
        secs % 60,
    );

    let mut out = String::new();
    if days > 0 {
        out.push_str(&format!("{days}d"));
    }
    if hours > 0 {
        out.push_str(&format!("{hours}h"));
    }
    if mins > 0 {
        out.push_str(&format!("{mins}m"));
    }
    out.push_str(&format!("{s}s"));
    out
}

fn config_label(path: &str) -> &str {
    Path::new(path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(path)
}

/// A row is one remote host. A file that did not load has no host, so the
/// filename is the row.
fn profile_title<'a>(b: &'a BackupReport, file: &'a str) -> &'a str {
    b.host.as_deref().or(b.profile.as_deref()).unwrap_or(file)
}

/// Name shown beside the host. A file that did not load has no profile name,
/// so the filename stays.
fn profile_label<'a>(b: &'a BackupReport, file: &'a str) -> &'a str {
    b.profile.as_deref().unwrap_or(file)
}

fn unloaded(b: &BackupReport) -> bool {
    b.host.is_none() && b.profile.is_none()
}

pub fn subject(report: &RunReport) -> String {
    let (ok, failed) = report.counts();
    let state = if report.has_errors() { "ERRORS" } else { "OK" };
    format!(
        "[znapr] {} backup {} ({} ok, {} failed)",
        report.hostname, state, ok, failed
    )
}

#[derive(Serialize)]
struct VolumeView {
    name: String,
    source: String,
    duration: String,
    failed: bool,
    vanished: bool,
    error: Option<String>,
}

#[derive(Serialize)]
struct BackupView {
    title: String,
    label: String,
    duration: String,
    unloaded: bool,
    error: Option<String>,
    volumes: Vec<VolumeView>,
}

#[derive(Serialize)]
struct MailView {
    hostname: String,
    started: String,
    duration: String,
    state: &'static str,
    ok: usize,
    failed: usize,
    accent: &'static str,
    /// Opaque blend of `accent` at ~12% over the card background `#0f172a`.
    /// An 8-digit hex (`#rrggbbaa`) makes Thunderbird drop the whole style.
    accent_bg: &'static str,
    extended: bool,
    backups: Vec<BackupView>,
}

fn trimmed(error: &Option<String>) -> Option<String> {
    error.as_ref().map(|err| err.trim().to_string())
}

fn mail_view(report: &RunReport, extended: bool) -> MailView {
    let (ok, failed) = report.counts();
    let error = report.has_errors();
    MailView {
        hostname: report.hostname.clone(),
        started: report.started.format("%Y-%m-%d %H:%M:%S").to_string(),
        duration: fmt_duration(report.duration),
        state: if error { "ERRORS" } else { "OK" },
        ok,
        failed,
        // Self-contained palette: the same colors in light and dark clients.
        accent: if error { "#ef4444" } else { "#22c55e" },
        accent_bg: if error { "#2b1d2e" } else { "#122d31" },
        extended,
        backups: report.backups.iter().map(backup_view).collect(),
    }
}

fn backup_view(backup: &BackupReport) -> BackupView {
    let file = config_label(&backup.config_file);
    BackupView {
        title: profile_title(backup, file).to_string(),
        label: profile_label(backup, file).to_string(),
        duration: fmt_duration(backup.duration),
        unloaded: unloaded(backup),
        error: trimmed(&backup.error),
        volumes: backup.volumes.iter().map(volume_view).collect(),
    }
}

fn volume_view(volume: &VolumeReport) -> VolumeView {
    VolumeView {
        name: volume.name.clone(),
        source: volume.source.clone(),
        duration: fmt_duration(volume.duration),
        failed: volume.status == Status::Failed,
        vanished: volume.vanished,
        error: trimmed(&volume.error),
    }
}

fn rjust(value: String, width: i64) -> String {
    format!("{value:>width$}", width = width.max(0) as usize)
}

fn ljust(value: String, width: i64) -> String {
    format!("{value:<width$}", width = width.max(0) as usize)
}

fn environment() -> &'static Environment<'static> {
    static ENV: OnceLock<Environment<'static>> = OnceLock::new();
    ENV.get_or_init(|| {
        let mut env = Environment::new();
        env.add_filter("rjust", rjust);
        env.add_filter("ljust", ljust);
        // Included at compile time: the binary does not read these at startup.
        env.add_template("report.txt", include_str!("../templates/mail/report.txt"))
            .expect("report.txt");
        env.add_template("report.html", include_str!("../templates/mail/report.html"))
            .expect("report.html");
        env
    })
}

fn render_template(name: &str, report: &RunReport, extended: bool) -> String {
    environment()
        .get_template(name)
        .unwrap_or_else(|err| panic!("{name}: {err}"))
        .render(mail_view(report, extended))
        .unwrap_or_else(|err| panic!("{name}: {err}"))
}

pub fn render_text(report: &RunReport, extended: bool) -> String {
    render_template("report.txt", report, extended)
}

pub fn render_html(report: &RunReport, extended: bool) -> String {
    render_template("report.html", report, extended)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(fail: bool) -> RunReport {
        RunReport {
            hostname: "host".to_string(),
            started: Local::now(),
            duration: Duration::from_secs(65),
            backups: vec![BackupReport {
                config_file: "web.yml".to_string(),
                profile: Some("web".to_string()),
                error: None,
                duration: Duration::from_secs(60),
                host: Some("web.example.com".to_string()),
                volumes: vec![VolumeReport {
                    name: "root".to_string(),
                    source: "/var/www".to_string(),
                    status: if fail { Status::Failed } else { Status::Ok },
                    error: if fail {
                        Some("rsync: connection refused".to_string())
                    } else {
                        None
                    },
                    duration: Duration::from_secs(59),
                    vanished: false,
                }],
            }],
        }
    }

    #[test]
    fn has_errors_reflects_volume_status() {
        assert!(!sample(false).has_errors());
        assert!(sample(true).has_errors());
    }

    #[test]
    fn renders_text_and_html_without_panicking() {
        let r = sample(true);
        assert!(render_text(&r, true).contains("[FAIL]"));
        let html = render_html(&r, true);
        assert!(html.contains("<html"));
        assert!(html.contains("rsync: connection refused"));
    }

    #[test]
    fn compact_marks_vanished_without_counting_it_as_an_error() {
        let vanished = RunReport {
            hostname: "backup.example.com".to_string(),
            started: Local::now(),
            duration: Duration::from_secs(134),
            backups: vec![BackupReport {
                config_file: "/etc/znapr/mail.yml".to_string(),
                profile: Some("mail".to_string()),
                host: Some("mail.example.com".to_string()),
                error: None,
                duration: Duration::from_secs(134),
                volumes: vec![
                    VolumeReport {
                        name: "spool".to_string(),
                        source: "/var/mail".to_string(),
                        status: Status::Ok,
                        error: None,
                        duration: Duration::from_secs(50),
                        vanished: false,
                    },
                    VolumeReport {
                        name: "archive".to_string(),
                        source: "/srv/archive".to_string(),
                        status: Status::Ok,
                        error: None,
                        duration: Duration::from_secs(40),
                        vanished: true,
                    },
                    VolumeReport {
                        name: "home".to_string(),
                        source: "/home".to_string(),
                        status: Status::Failed,
                        error: Some("rsync: connection refused".to_string()),
                        duration: Duration::from_secs(44),
                        vanished: false,
                    },
                ],
            }],
        };

        assert!(vanished.has_errors());
        let mut accepted = vanished.clone();
        accepted.backups[0].volumes.pop();
        assert!(!accepted.has_errors());
        assert_eq!(accepted.counts(), (2, 0));

        let text = render_text(&vanished, false);
        assert!(text.contains("* mail.example.com  (mail)  2m14s\n"));
        assert!(text.contains("    spool archive~ home!\n"));
        assert!(text.contains("    home: rsync: connection refused\n"));
        assert!(text.contains("name~ vanished files accepted"));
        let html = render_html(&vanished, false);
        assert!(html.contains("#f59e0b"));
        assert!(html.contains("background-color:#272527"));
        assert!(!html.contains("#f59e0b1a"));

        let extended = render_text(&vanished, true);
        assert!(extended.contains("[ VAN]"));
        assert!(extended.contains("[FAIL]"));
        assert!(!extended.contains("name~"));
    }
}
