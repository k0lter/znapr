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

/// Domain of the default sender. A trimmed `hostname -f` result wins; otherwise
/// the short name already stored on the report.
fn sender_domain(fqdn: Option<&str>, fallback: &str) -> String {
    fqdn.map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or(fallback)
        .to_string()
}

/// `hostname -f` is the FQDN on Linux. FreeBSD accepts `-f` and prints the name
/// set at boot (`hostname=` in rc.conf), without a DNS lookup.
fn hostname_fqdn() -> Option<String> {
    let output = Command::new("hostname").arg("-f").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8(output.stdout).ok()?;
    let name = name.trim();
    if name.is_empty() {
        None
    } else {
        Some(name.to_string())
    }
}

/// Explicit `-F` replaces the whole From. Blank falls back to `znapr@<fqdn>`.
fn report_from(override_from: Option<&str>, fallback_host: &str) -> String {
    if let Some(from) = override_from.map(str::trim).filter(|s| !s.is_empty()) {
        return from.to_string();
    }
    format!(
        "znapr@{}",
        sender_domain(hostname_fqdn().as_deref(), fallback_host)
    )
}

/// Addr-spec for `sendmail -f`. `Name <addr>` keeps only `addr`.
fn envelope_sender(from: &str) -> &str {
    if let (Some(open), Some(close)) = (from.rfind('<'), from.rfind('>'))
        && close > open
        && let addr = from[open + 1..close].trim()
        && !addr.is_empty()
    {
        return addr;
    }
    from.trim()
}

fn message_domain<'a>(from: &'a str, fallback: &'a str) -> &'a str {
    from.rsplit_once('@')
        .map(|(_, domain)| domain.trim().trim_end_matches('>').trim())
        .filter(|domain| !domain.is_empty())
        .unwrap_or(fallback)
}

fn build_message(from: &str, to: &str, report: &RunReport, extended: bool) -> String {
    let subject = report::subject(report);
    let text = report::render_text(report, extended);
    let html = report::render_html(report, extended);
    let (pid, nanos) = message_stamp();
    let boundary = format!("znapr_{pid}_{nanos}");
    let domain = message_domain(from, &report.hostname);

    let mut m = String::new();
    m.push_str(&format!("From: {from}\r\n"));
    m.push_str(&format!("To: {to}\r\n"));
    m.push_str(&format!("Subject: {subject}\r\n"));
    m.push_str(&format!("Date: {}\r\n", report.started.to_rfc2822()));
    m.push_str(&format!("Message-ID: <znapr.{pid}.{nanos}@{domain}>\r\n"));
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
pub fn send(report: &RunReport, to: &str, from: Option<&str>, extended: bool) -> Result<()> {
    let from = report_from(from, &report.hostname);
    let message = build_message(&from, to, report, extended);

    let envelope = envelope_sender(&from);
    let mut child = Command::new(sendmail_path())
        .args(["-t", "-i", "-f", envelope])
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
                host: Some("web.example.com".to_string()),
                volumes: vec![VolumeReport {
                    name: "root".to_string(),
                    source: "/var/www".to_string(),
                    status: Status::Ok,
                    error: None,
                    duration: Duration::from_secs(1),
                    vanished: false,
                }],
            }],
        };

        let msg = build_message("znapr@host", "ops@example.com", &report, false);
        assert!(msg.contains("To: ops@example.com\r\n"));
        assert!(msg.contains("Content-Type: multipart/alternative"));
        assert!(msg.contains("Content-Type: text/plain; charset=UTF-8"));
        assert!(msg.contains("Content-Type: text/html; charset=UTF-8"));
        assert!(msg.contains("Auto-Submitted: auto-generated\r\n"));
        assert!(msg.contains("Message-ID: <znapr."));
        assert!(msg.contains("@host>\r\n"));

        let overridden = build_message(
            "Backup <znapr@backup.example.com>",
            "ops@example.com",
            &report,
            false,
        );
        assert!(overridden.contains("From: Backup <znapr@backup.example.com>\r\n"));
        assert!(overridden.contains("@backup.example.com>\r\n"));
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
    fn sender_domain_prefers_a_trimmed_fqdn() {
        assert_eq!(
            sender_domain(Some("  backup.example.com  "), "host"),
            "backup.example.com"
        );
        assert_eq!(sender_domain(Some("  "), "host"), "host");
        assert_eq!(sender_domain(None, "host"), "host");
    }

    #[test]
    fn report_from_keeps_an_explicit_address() {
        assert_eq!(
            report_from(Some("  ops@example.com  "), "host"),
            "ops@example.com"
        );
        assert_eq!(report_from(Some("   "), "host"), report_from(None, "host"));
    }

    #[test]
    fn envelope_sender_uses_the_address_inside_brackets() {
        assert_eq!(envelope_sender("znapr@host"), "znapr@host");
        assert_eq!(envelope_sender("Nom <znapr@host>"), "znapr@host");
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
