use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;

use crate::report::{self, RunReport};

const SENDMAIL: &str = "/usr/sbin/sendmail";

fn sendmail_path() -> String {
    std::env::var("ZNAPR_SENDMAIL").unwrap_or_else(|_| SENDMAIL.to_string())
}

fn base64_wrapped(data: &str) -> String {
    let encoded = STANDARD.encode(data.as_bytes());
    encoded
        .as_bytes()
        .chunks(76)
        .map(|c| std::str::from_utf8(c).unwrap_or_default())
        .collect::<Vec<_>>()
        .join("\r\n")
}

fn boundary() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("znapr_{}_{}", std::process::id(), nanos)
}

fn build_message(from: &str, to: &str, report: &RunReport) -> String {
    let subject = report::subject(report);
    let text = report::render_text(report);
    let html = report::render_html(report);
    let boundary = boundary();

    let mut m = String::new();
    m.push_str(&format!("From: {from}\r\n"));
    m.push_str(&format!("To: {to}\r\n"));
    m.push_str(&format!("Subject: {subject}\r\n"));
    m.push_str(&format!("Date: {}\r\n", report.started.to_rfc2822()));
    m.push_str("MIME-Version: 1.0\r\n");
    m.push_str(&format!(
        "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n"
    ));
    m.push_str("This is a multi-part message in MIME format.\r\n");

    m.push_str(&format!("--{boundary}\r\n"));
    m.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    m.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
    m.push_str(&base64_wrapped(&text));
    m.push_str("\r\n");

    m.push_str(&format!("--{boundary}\r\n"));
    m.push_str("Content-Type: text/html; charset=UTF-8\r\n");
    m.push_str("Content-Transfer-Encoding: base64\r\n\r\n");
    m.push_str(&base64_wrapped(&html));
    m.push_str("\r\n");

    m.push_str(&format!("--{boundary}--\r\n"));
    m
}

/// Send the report as a multipart/alternative (text + HTML) e-mail via the
/// local MTA's sendmail interface.
pub fn send(report: &RunReport, to: &str) -> Result<()> {
    let from = format!("znapr@{}", report.hostname);
    let message = build_message(&from, to, report);

    let mut child = Command::new(sendmail_path())
        .args(["-t", "-i"])
        .stdin(Stdio::piped())
        .spawn()
        .context("Failed to spawn sendmail (is an MTA installed?)")?;

    child
        .stdin
        .take()
        .expect("sendmail stdin piped")
        .write_all(message.as_bytes())
        .context("Failed to write message to sendmail")?;

    let status = child.wait().context("sendmail did not complete")?;
    if !status.success() {
        bail!("sendmail exited with status {}", status.code().unwrap_or(-1));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Local;

    use super::*;
    use crate::report::{BackupReport, RunReport, Status, VolumeReport};

    #[test]
    fn build_message_is_multipart_with_both_parts() {
        let report = RunReport {
            hostname: "host".to_string(),
            started: Local::now(),
            duration: Duration::from_secs(1),
            backups: vec![BackupReport {
                config_file: "web.yml".to_string(),
                profile: Some("web".to_string()),
                error: None,
                duration: Duration::from_secs(1),
                volumes: vec![VolumeReport {
                    name: "root".to_string(),
                    source: "/var/www".to_string(),
                    status: Status::Ok,
                    error: None,
                    duration: Duration::from_secs(1),
                }],
            }],
        };

        let msg = build_message("znapr@host", "ops@example.com", &report);
        assert!(msg.contains("To: ops@example.com\r\n"));
        assert!(msg.contains("Content-Type: multipart/alternative"));
        assert!(msg.contains("Content-Type: text/plain; charset=UTF-8"));
        assert!(msg.contains("Content-Type: text/html; charset=UTF-8"));
        // Base64 parts + closing boundary present.
        assert_eq!(msg.matches("Content-Transfer-Encoding: base64").count(), 2);
        assert!(msg.trim_end().ends_with("--"));
    }
}
