use std::path::Path;
use std::time::Duration;

use chrono::{DateTime, Local};

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

fn badge(v: &VolumeReport) -> &'static str {
    match v.status {
        Status::Failed => "FAIL",
        Status::Ok if v.vanished => "VAN",
        Status::Ok => "OK",
    }
}

fn volume_color(v: &VolumeReport) -> &'static str {
    match v.status {
        Status::Failed => "#ef4444",
        Status::Ok if v.vanished => "#f59e0b",
        Status::Ok => "#22c55e",
    }
}

fn text_suffix(v: &VolumeReport) -> &'static str {
    match v.status {
        Status::Failed => "!",
        Status::Ok if v.vanished => "~",
        Status::Ok => "",
    }
}

pub fn subject(report: &RunReport) -> String {
    let (ok, failed) = report.counts();
    let state = if report.has_errors() { "ERRORS" } else { "OK" };
    format!(
        "[znapr] {} backup {} ({} ok, {} failed)",
        report.hostname, state, ok, failed
    )
}

fn push_text_header(out: &mut String, report: &RunReport) {
    let (ok, failed) = report.counts();
    let state = if report.has_errors() { "ERRORS" } else { "OK" };
    out.push_str(&format!("znapr backup report - {}\n", report.hostname));
    out.push_str(&format!(
        "Started : {}\n",
        report.started.format("%Y-%m-%d %H:%M:%S")
    ));
    out.push_str(&format!("Duration: {}\n", fmt_duration(report.duration)));
    out.push_str(&format!("Status  : {state}  ({ok} ok, {failed} failed)\n"));
    out.push('\n');
}

fn push_profile_heading(out: &mut String, b: &BackupReport) {
    let file = config_label(&b.config_file);
    if unloaded(b) {
        out.push_str(&format!("* {file}\n"));
        return;
    }
    let label = profile_label(b, file);
    out.push_str(&format!(
        "* {}  ({label})  {}\n",
        profile_title(b, file),
        fmt_duration(b.duration)
    ));
}

pub fn render_text(report: &RunReport, extended: bool) -> String {
    let mut out = String::new();
    push_text_header(&mut out, report);

    for b in &report.backups {
        push_profile_heading(&mut out, b);

        if let Some(err) = &b.error {
            out.push_str(&format!("    ! {}\n", err.trim()));
        }

        if extended {
            for v in &b.volumes {
                out.push_str(&format!(
                    "    [{:>4}] {:<20} {:>8}   {}\n",
                    badge(v),
                    v.name,
                    fmt_duration(v.duration),
                    v.source
                ));
                if let Some(err) = &v.error {
                    out.push_str(&format!("           -> {}\n", err.trim()));
                }
            }
        } else if !b.volumes.is_empty() {
            let names: Vec<String> = b
                .volumes
                .iter()
                .map(|v| format!("{}{}", v.name, text_suffix(v)))
                .collect();
            out.push_str(&format!("    {}\n", names.join(" ")));
            for v in &b.volumes {
                if v.status == Status::Failed
                    && let Some(err) = &v.error
                {
                    out.push_str(&format!("    {}: {}\n", v.name, err.trim()));
                }
            }
        }
        out.push('\n');
    }

    if !extended {
        out.push_str("Volumes: name ok, name~ vanished files accepted, name! failed.\n");
    }

    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub fn render_html(report: &RunReport, extended: bool) -> String {
    // Self-contained dark card with an explicit palette: renders consistently
    // in both light and dark clients (we set our own background/foreground).
    let (ok, failed) = report.counts();
    let error = report.has_errors();
    let accent = if error { "#ef4444" } else { "#22c55e" };
    let state = if error { "ERRORS" } else { "OK" };

    let rows = if extended {
        extended_rows(report)
    } else {
        compact_rows(report)
    };

    format!(
        r#"<!doctype html>
<html>
<head><meta charset="utf-8"><meta name="color-scheme" content="light dark"><meta name="supported-color-schemes" content="light dark"></head>
<body style="margin:0;padding:24px;background:#020617;color-scheme:light dark;font-family:-apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;">
  <table role="presentation" cellpadding="0" cellspacing="0" width="100%" style="max-width:640px;margin:0 auto;background:#0f172a;border:1px solid #1e293b;border-radius:14px;overflow:hidden;">
    <tr>
      <td style="padding:20px 16px;border-left:4px solid {accent};background:#0b1220;">
        <div style="font-size:18px;font-weight:700;color:#f8fafc;">znapr backup report</div>
        <div style="font-size:13px;color:#94a3b8;margin-top:4px;">{host} &nbsp;&middot;&nbsp; {started}</div>
        <div style="margin-top:12px;">
          <span style="display:inline-block;padding:3px 10px;border-radius:999px;font-size:12px;font-weight:700;color:{accent};background:{accent}1f;">{state}</span>
          <span style="color:#22c55e;font-size:13px;margin-left:8px;">{ok} ok</span>
          <span style="color:#ef4444;font-size:13px;margin-left:8px;">{failed} failed</span>
          <span style="color:#64748b;font-size:13px;margin-left:8px;">in {dur}</span>
        </div>
      </td>
    </tr>
    <tr><td style="padding:0;">
      <table role="presentation" cellpadding="0" cellspacing="0" width="100%">{rows}</table>
    </td></tr>
  </table>
  <div style="max-width:640px;margin:12px auto 0;color:#475569;font-size:11px;text-align:center;">Generated by znapr</div>
</body>
</html>"#,
        accent = accent,
        host = esc(&report.hostname),
        started = esc(&report.started.format("%Y-%m-%d %H:%M:%S").to_string()),
        state = state,
        ok = ok,
        failed = failed,
        dur = esc(&fmt_duration(report.duration)),
        rows = rows,
    )
}

fn pill(v: &VolumeReport) -> String {
    let color = volume_color(v);
    format!(
        r#"<span style="display:inline-block;margin:2px 6px 2px 0;padding:2px 8px;border-radius:999px;font-size:12px;font-weight:600;color:{color};background:{color}1a;">{}</span>"#,
        esc(&v.name)
    )
}

fn compact_rows(report: &RunReport) -> String {
    let mut rows = String::new();
    for (i, b) in report.backups.iter().enumerate() {
        let file = config_label(&b.config_file);
        let title = esc(profile_title(b, file));
        let border = if i == 0 {
            String::new()
        } else {
            "border-top:1px solid #1e293b;".to_string()
        };
        let (subtitle, dur) = if unloaded(b) {
            ("profile did not load".to_string(), String::new())
        } else {
            (profile_label(b, file).to_string(), fmt_duration(b.duration))
        };
        let pills: String = b.volumes.iter().map(pill).collect();
        rows.push_str(&format!(
            r#"<tr>
                 <td style="padding:8px 16px;{border}width:210px;vertical-align:top;"><div style="font-size:14px;font-weight:600;color:#f1f5f9;">{title}</div><div style="font-size:11px;color:#64748b;">{subtitle}</div></td>
                 <td style="padding:8px;{border}width:56px;font-size:12px;color:#64748b;vertical-align:top;">{dur}</td>
                 <td style="padding:8px 16px 8px 8px;{border}">{pills}</td>
               </tr>"#,
            subtitle = esc(&subtitle),
            dur = esc(&dur),
        ));
        for v in &b.volumes {
            if v.status == Status::Failed
                && let Some(err) = &v.error
            {
                rows.push_str(&format!(
                    r#"<tr><td></td><td></td><td style="padding:0 16px 8px 8px;font-family:ui-monospace,Menlo,Consolas,monospace;font-size:12px;color:#fca5a5;">{}: {}</td></tr>"#,
                    esc(&v.name),
                    esc(err.trim())
                ));
            }
        }
        if let Some(err) = &b.error {
            rows.push_str(&format!(
                r#"<tr><td></td><td></td><td style="padding:0 16px 8px 8px;font-family:ui-monospace,Menlo,Consolas,monospace;font-size:12px;color:#fca5a5;">{}</td></tr>"#,
                esc(err.trim())
            ));
        }
    }
    rows
}

fn extended_rows(report: &RunReport) -> String {
    let mut rows = String::new();
    for (i, b) in report.backups.iter().enumerate() {
        let file = config_label(&b.config_file);
        let title = esc(profile_title(b, file));
        let border = if i == 0 {
            String::new()
        } else {
            "border-top:1px solid #1e293b;".to_string()
        };
        let meta = if unloaded(b) {
            "profile did not load".to_string()
        } else {
            format!(
                "{} &nbsp;&middot;&nbsp; {}",
                profile_label(b, file),
                fmt_duration(b.duration)
            )
        };
        rows.push_str(&format!(
            r#"<tr><td colspan="3" style="padding:14px 16px 4px;{border}font-weight:600;font-size:15px;color:#f1f5f9;">{title} <span style="color:#64748b;font-weight:400;font-size:12px;">{meta}</span></td></tr>"#,
        ));

        if let Some(err) = &b.error {
            rows.push_str(&format!(
                r#"<tr><td colspan="3" style="padding:2px 16px 8px;color:#fca5a5;font-size:12px;font-family:ui-monospace,Menlo,Consolas,monospace;">{err}</td></tr>"#,
                err = esc(err.trim())
            ));
        }

        for v in &b.volumes {
            let color = volume_color(v);
            rows.push_str(&format!(
                r#"<tr>
                     <td style="padding:6px 16px;white-space:nowrap;">
                       <span style="display:inline-block;min-width:44px;text-align:center;padding:2px 8px;border-radius:6px;font-size:12px;font-weight:600;color:{color};background:{color}1a;">{status}</span>
                     </td>
                     <td style="padding:6px 16px;color:#e2e8f0;font-size:14px;">{name}<div style="color:#64748b;font-size:12px;">{source}</div></td>
                     <td style="padding:6px 16px;color:#94a3b8;font-size:13px;text-align:right;white-space:nowrap;">{dur}</td>
                   </tr>"#,
                status = badge(v),
                name = esc(&v.name),
                source = esc(&v.source),
                dur = esc(&fmt_duration(v.duration)),
            ));
            if let Some(err) = &v.error {
                rows.push_str(&format!(
                    r#"<tr><td></td><td colspan="2" style="padding:0 16px 8px;color:#fca5a5;font-size:12px;font-family:ui-monospace,Menlo,Consolas,monospace;white-space:pre-wrap;">{}</td></tr>"#,
                    esc(err.trim())
                ));
            }
        }
    }
    rows
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
        assert!(render_html(&vanished, false).contains("#f59e0b"));

        let extended = render_text(&vanished, true);
        assert!(extended.contains("[ VAN]"));
        assert!(extended.contains("[FAIL]"));
        assert!(!extended.contains("name~"));
    }
}
