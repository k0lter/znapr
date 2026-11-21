use std::time::Duration;

use chrono::{DateTime, Local};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Failed,
}

pub struct VolumeReport {
    pub name: String,
    pub source: String,
    pub status: Status,
    pub error: Option<String>,
    pub duration: Duration,
}

pub struct BackupReport {
    pub config_file: String,
    pub profile: Option<String>,
    /// Config-level failure (e.g. the YAML could not be loaded/validated).
    pub error: Option<String>,
    pub volumes: Vec<VolumeReport>,
    pub duration: Duration,
}

pub struct RunReport {
    pub hostname: String,
    pub started: DateTime<Local>,
    pub duration: Duration,
    pub backups: Vec<BackupReport>,
}

impl RunReport {
    pub fn has_errors(&self) -> bool {
        self.backups.iter().any(|b| {
            b.error.is_some() || b.volumes.iter().any(|v| v.status == Status::Failed)
        })
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

fn status_label(s: Status) -> &'static str {
    match s {
        Status::Ok => "OK",
        Status::Failed => "FAIL",
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

pub fn render_text(report: &RunReport) -> String {
    let mut out = String::new();
    let (ok, failed) = report.counts();
    let state = if report.has_errors() { "ERRORS" } else { "OK" };

    out.push_str(&format!("znapr backup report - {}\n", report.hostname));
    out.push_str(&format!(
        "Started : {}\n",
        report.started.format("%Y-%m-%d %H:%M:%S")
    ));
    out.push_str(&format!("Duration: {}\n", fmt_duration(report.duration)));
    out.push_str(&format!(
        "Status  : {state}  ({ok} ok, {failed} failed)\n"
    ));
    out.push('\n');

    for b in &report.backups {
        let title = b.profile.as_deref().unwrap_or(&b.config_file);
        out.push_str(&format!(
            "* {}  ({})  {}\n",
            title,
            b.config_file,
            fmt_duration(b.duration)
        ));

        if let Some(err) = &b.error {
            out.push_str(&format!("    ! {err}\n"));
        }

        for v in &b.volumes {
            out.push_str(&format!(
                "    [{:>4}] {:<20} {:>8}   {}\n",
                status_label(v.status),
                v.name,
                fmt_duration(v.duration),
                v.source
            ));
            if let Some(err) = &v.error {
                out.push_str(&format!("           -> {}\n", err.trim()));
            }
        }
        out.push('\n');
    }

    out
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn status_color(s: Status) -> &'static str {
    match s {
        Status::Ok => "#22c55e",
        Status::Failed => "#ef4444",
    }
}

pub fn render_html(report: &RunReport) -> String {
    // Self-contained dark card with an explicit palette: renders consistently
    // in both light and dark clients (we set our own background/foreground).
    let (ok, failed) = report.counts();
    let error = report.has_errors();
    let accent = if error { "#ef4444" } else { "#22c55e" };
    let state = if error { "ERRORS" } else { "OK" };

    let mut rows = String::new();
    for b in &report.backups {
        let title = esc(b.profile.as_deref().unwrap_or(&b.config_file));
        rows.push_str(&format!(
            r#"<tr><td colspan="3" style="padding:16px 16px 4px;font-weight:600;font-size:15px;color:#f1f5f9;">
                 {title}
                 <span style="color:#64748b;font-weight:400;font-size:12px;"> &nbsp;{}</span>
               </td></tr>"#,
            esc(&fmt_duration(b.duration)),
        ));

        if let Some(err) = &b.error {
            rows.push_str(&format!(
                r#"<tr><td colspan="3" style="padding:2px 16px 8px;color:#ef4444;font-size:13px;">{}</td></tr>"#,
                esc(err.trim())
            ));
        }

        for v in &b.volumes {
            let color = status_color(v.status);
            rows.push_str(&format!(
                r#"<tr>
                     <td style="padding:6px 16px;white-space:nowrap;">
                       <span style="display:inline-block;min-width:52px;text-align:center;padding:2px 8px;border-radius:6px;font-size:12px;font-weight:600;color:{color};background:{color}1a;">{status}</span>
                     </td>
                     <td style="padding:6px 16px;color:#e2e8f0;font-size:14px;">{name}<div style="color:#64748b;font-size:12px;">{source}</div></td>
                     <td style="padding:6px 16px;color:#94a3b8;font-size:13px;text-align:right;white-space:nowrap;">{dur}</td>
                   </tr>"#,
                color = color,
                status = status_label(v.status),
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
        assert!(render_text(&r).contains("FAIL"));
        let html = render_html(&r);
        assert!(html.contains("<html"));
        assert!(html.contains("rsync: connection refused"));
    }
}
