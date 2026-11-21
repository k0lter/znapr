use std::io::Write;
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};

use crate::report::{self, RunReport};

const SENDMAIL: &str = "/usr/sbin/sendmail";

fn sendmail_path() -> String {
    std::env::var("ZNAPR_SENDMAIL").unwrap_or_else(|_| SENDMAIL.to_string())
}

/// Quoted-printable for a UTF-8 body (RFC 2045). Lines stay at most 76
/// characters; this is not a decoder. Upgrade path: a MIME crate if we grow
/// attachments or nested parts.
fn quoted_printable(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::new();
    let mut col = 0usize;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\n' || (bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n')) {
            out.push_str("\r\n");
            col = 0;
            i += if bytes[i] == b'\r' { 2 } else { 1 };
            continue;
        }

        let b = bytes[i];
        // Space and tab stay literal unless they end a line, where a transport
        // may strip them. `=` always starts an escape.
        let literal = match b {
            b' ' | b'\t' => !matches!(bytes.get(i + 1), None | Some(b'\n' | b'\r')),
            b'=' => false,
            _ => b.is_ascii_graphic(),
        };
        let token_len = if literal { 1 } else { 3 };
        // The soft-break '=' counts toward the 76-character limit.
        if col > 0 && col + token_len > 75 {
            out.push_str("=\r\n");
            col = 0;
        }
        if literal {
            out.push(b as char);
            col += 1;
        } else {
            out.push_str(&format!("={b:02X}"));
            col += 3;
        }
        i += 1;
    }
    out
}

fn message_stamp() -> (u32, u128) {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (std::process::id(), nanos)
}

fn build_message(from: &str, to: &str, report: &RunReport) -> String {
    let subject = report::subject(report);
    let text = report::render_text(report);
    let html = report::render_html(report);
    let (pid, nanos) = message_stamp();
    let boundary = format!("znapr_{pid}_{nanos}");

    let mut m = String::new();
    m.push_str(&format!("From: {from}\r\n"));
    m.push_str(&format!("To: {to}\r\n"));
    m.push_str(&format!("Subject: {subject}\r\n"));
    m.push_str(&format!("Date: {}\r\n", report.started.to_rfc2822()));
    m.push_str(&format!(
        "Message-ID: <znapr.{pid}.{nanos}@{}>\r\n",
        report.hostname
    ));
    m.push_str("Auto-Submitted: auto-generated\r\n");
    m.push_str("MIME-Version: 1.0\r\n");
    m.push_str(&format!(
        "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n"
    ));
    m.push_str("This is a multi-part message in MIME format.\r\n");

    m.push_str(&format!("--{boundary}\r\n"));
    m.push_str("Content-Type: text/plain; charset=UTF-8\r\n");
    m.push_str("Content-Transfer-Encoding: quoted-printable\r\n\r\n");
    m.push_str(&quoted_printable(&text));
    m.push_str("\r\n");

    m.push_str(&format!("--{boundary}\r\n"));
    m.push_str("Content-Type: text/html; charset=UTF-8\r\n");
    m.push_str("Content-Transfer-Encoding: quoted-printable\r\n\r\n");
    m.push_str(&quoted_printable(&html));
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
        bail!(
            "sendmail exited with status {}",
            status.code().unwrap_or(-1)
        );
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
        assert!(msg.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(msg.contains("Message-ID: <znapr."));
        assert!(msg.contains("@host>\r\n"));
        assert!(msg.contains("znapr backup report"));
        assert_eq!(
            msg.matches("Content-Transfer-Encoding: quoted-printable")
                .count(),
            2
        );
        assert!(!msg.contains("base64"));
        assert!(msg.trim_end().ends_with("--"));
    }

    #[test]
    fn quoted_printable_encodes_utf8_trailing_space_and_long_lines() {
        assert_eq!(quoted_printable("é"), "=C3=A9");
        assert_eq!(quoted_printable("a \n"), "a=20\r\n");
        let long = "a".repeat(80);
        let encoded = quoted_printable(&long);
        assert!(encoded.contains("=\r\n"));
        assert!(encoded.lines().all(|l| l.len() <= 76));
    }
}
