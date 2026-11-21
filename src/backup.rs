use std::fs;
use std::io::{IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use chrono::Local;
use tracing::{debug, error, info, trace};

use crate::config::{Config, SshParams, Volume};
use crate::logger::{enabled_debug, progress_enabled};
use crate::progress::BackupProgress;
use crate::report::{BackupReport, RunReport, Status, VolumeReport};

pub(crate) struct CmdResult {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// Invoke an external command. When `capture` is false the child inherits the
/// terminal (used so rsync can stream its own progress directly).
pub(crate) fn call(args: &[&str], capture: bool) -> CmdResult {
    let cmd_str = args.join(" ");
    trace!("Invoking: [data]{}[/]", cmd_str);

    let mut command = Command::new(args[0]);
    command.args(&args[1..]);

    let result = if capture {
        command.output().map(|o| CmdResult {
            code: o.status.code().unwrap_or(-1),
            stdout: String::from_utf8_lossy(&o.stdout).to_string(),
            stderr: String::from_utf8_lossy(&o.stderr).to_string(),
        })
    } else {
        command.status().map(|s| CmdResult {
            code: s.code().unwrap_or(-1),
            stdout: String::new(),
            stderr: String::new(),
        })
    };

    match result {
        Ok(r) => {
            trace!(
                "Command result: rc=[result]{}[/] stdout=[data]{}[/] stderr=[data]{}[/]",
                r.code, r.stdout, r.stderr
            );

            r
        }
        Err(e) => {
            error!("Error while invoking \"[data]{}[/]\": {}", cmd_str, e);

            CmdResult {
                code: -1,
                stdout: String::new(),
                stderr: String::new(),
            }
        }
    }
}

fn has_zfsvol(vol: &str) -> bool {
    let r = call(&["zfs", "list", "-H", "-o", "name", vol], true);

    r.code == 0 && r.stdout.trim() == vol
}

fn create_zfsvol(vol: &str) -> bool {
    call(&["zfs", "create", vol], true).code == 0
}

fn get_zfsvol_mountpoint(vol: &str) -> Option<String> {
    let r = call(&["zfs", "list", "-H", "-o", "mountpoint", vol], true);

    if r.code == 0 && r.stdout.starts_with('/') {
        Some(r.stdout.trim().to_string())
    } else {
        None
    }
}

fn ensure_zfsvol(vol: &str) {
    if !has_zfsvol(vol) {
        info!("Creating ZFS volume [cfg]{}[/]", vol);

        if create_zfsvol(vol) {
            info!("Volume [cfg]{}[/] successfully created", vol);
        } else {
            error!("Unable to create volume [data]{}[/]", vol);
        }
    }
}

/// Parse rsync's `--info=progress2` overall percentage from one output segment.
fn parse_percent(segment: &[u8]) -> Option<u8> {
    std::str::from_utf8(segment)
        .ok()?
        .split_whitespace()
        .find_map(|tok| tok.strip_suffix('%')?.parse::<u16>().ok())
        .map(|p| p.min(100) as u8)
}

/// Run rsync, capturing stderr for the report. When `parse` is set, stdout is
/// piped and its `--info=progress2` percentages drive `on_percent`; otherwise
/// stdout is inherited (so raw progress can stream at -vv, or stay silent).
fn run_rsync(args: &[&str], parse: bool, on_percent: &mut dyn FnMut(u8)) -> (i32, String) {
    trace!("Invoking: [data]{}[/]", args.join(" "));

    let mut command = Command::new(args[0]);
    command.args(&args[1..]).stderr(Stdio::piped());
    if parse {
        command.stdout(Stdio::piped());
    }

    let mut child = match command.spawn() {
        Ok(c) => c,
        Err(e) => {
            error!("Error while invoking rsync: {}", e);

            return (-1, e.to_string());
        }
    };

    // Drain stderr on a thread so a full pipe buffer can never deadlock us.
    let stderr = child.stderr.take().expect("stderr piped");
    let err_handle = std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = std::io::BufReader::new(stderr).read_to_string(&mut buf);
        buf
    });

    if parse {
        let stdout = child.stdout.take().expect("stdout piped");
        let mut reader = std::io::BufReader::new(stdout);
        let mut segment: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        // rsync separates in-place progress updates with '\r'.
        while let Ok(1) = reader.read(&mut byte) {
            match byte[0] {
                b'\r' | b'\n' => {
                    if let Some(p) = parse_percent(&segment) {
                        on_percent(p);
                    }
                    segment.clear();
                }
                b => segment.push(b),
            }
        }
        if let Some(p) = parse_percent(&segment) {
            on_percent(p);
        }
    }

    let code = child.wait().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    let stderr = err_handle.join().unwrap_or_default();
    trace!(
        "Command result: rc=[result]{}[/] stderr=[data]{}[/]",
        code, stderr
    );

    (code, stderr)
}

/// Returns `Some(error)` on failure, `None` on success.
fn backup_data(
    ssh: &SshParams,
    source: &str,
    dest: &str,
    excludes: Option<&[String]>,
    label: &str,
    progress: &mut BackupProgress,
) -> Option<String> {
    let mut ssh_args = format!("-o StrictHostKeyChecking=accept-new -p {}", ssh.port);
    if let Some(key) = ssh.sshkey {
        ssh_args.push_str(&format!(" -i {}", key));
    }

    let ssh_opt = format!("ssh {}", ssh_args);
    let mut args: Vec<String> = vec![
        "rsync".into(),
        "-e".into(),
        ssh_opt,
        "--delete".into(),
        "--archive".into(),
    ];

    // Drive the progress bar (default level, tty), or stream raw progress at -vv.
    let bar = progress_enabled();
    let stream_raw = !bar && enabled_debug() && std::io::stdout().is_terminal();
    if bar || stream_raw {
        args.push("--info=progress2".into());
        args.push("--no-inc-recursive".into());
    }

    if let Some(excludes) = excludes {
        for e in excludes {
            args.push("--exclude".into());
            args.push(e.clone());
        }
    }

    let src = format!("{}:{}/", ssh.host, source.trim_end_matches('/'));
    let dst = format!("{}/", dest.trim_end_matches('/'));
    args.push(src.clone());
    args.push(dst.clone());

    info!("Syncing data from [cfg]{}[/] to [cfg]{}[/]", src, dst);

    let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, stderr) = if bar {
        let id = progress.start_rsync(&format!("rsync {}", label)).unwrap();
        let (code, stderr) = {
            let mut cb = |pct| progress.set_rsync(id, pct);
            run_rsync(&arg_refs, true, &mut cb)
        };
        progress.end_rsync(id);
        (code, stderr)
    } else {
        run_rsync(&arg_refs, false, &mut |_| {})
    };

    if code == 0 {
        return None;
    }

    if stderr.trim().is_empty() {
        error!(
            "Something went wrong while trying to sync data from [cfg]{}[/] to [cfg]{}[/]",
            src, dst
        );

        Some(format!("rsync exited with status {code}"))
    } else {
        error!(
            "[data]Something went wrong while trying to sync data from {} to {}[/]: [err]{}[/]",
            src, dst, stderr
        );

        Some(stderr.trim().to_string())
    }
}

/// Returns `Some(error)` on failure, `None` on success.
fn backup_snapshot(vol: &str, days: i64) -> Option<String> {
    let date = Local::now().format("%Y-%m-%d_%H.%M.%S");
    let snapshot = format!("{}@{}--{}d", vol, date, days);
    let r = call(&["zfs", "snapshot", &snapshot], true);

    if r.code != 0 {
        error!(
            "Something went wrong while creating zfs snapshot for [data]{}[/]: [err]{}[/]",
            vol, r.stderr
        );

        return Some(if r.stderr.trim().is_empty() {
            format!("zfs snapshot exited with status {}", r.code)
        } else {
            r.stderr.trim().to_string()
        });
    }

    None
}

fn backup_vol(
    ssh: &SshParams,
    parent: &str,
    name: &str,
    settings: &Volume,
    progress: &mut BackupProgress,
) -> VolumeReport {
    let start = Instant::now();
    let zfsvol = format!("{}/{}", parent, name);
    ensure_zfsvol(&zfsvol);

    let mut report = VolumeReport {
        name: name.to_string(),
        source: settings.source.clone(),
        status: Status::Failed,
        error: None,
        duration: Duration::default(),
    };

    let dest = match get_zfsvol_mountpoint(&zfsvol) {
        Some(d) => {
            debug!("Found mountpoint for [cfg]{}[/]: [cfg]{}[/]", zfsvol, d);

            d
        }
        None => {
            error!("Unable to find mountpoint for [data]{}[/]", zfsvol);
            report.error = Some(format!("Unable to find mountpoint for {zfsvol}"));
            report.duration = start.elapsed();

            return report;
        }
    };

    if let Some(err) = backup_data(
        ssh,
        &settings.source,
        &dest,
        settings.excludes.as_deref(),
        name,
        progress,
    ) {
        report.error = Some(err);
        report.duration = start.elapsed();

        return report;
    }

    if let Some(err) = backup_snapshot(&zfsvol, settings.max_days) {
        report.error = Some(err);
        report.duration = start.elapsed();

        return report;
    }

    report.status = Status::Ok;
    report.duration = start.elapsed();

    report
}

fn backup(path: &Path, cfg: Config, progress: &mut BackupProgress) -> BackupReport {
    let start = Instant::now();
    let root_vol = format!("{}/{}", cfg.zfsroot, cfg.name);
    ensure_zfsvol(&root_vol);

    let ssh = cfg.ssh_params();
    let mut volumes = Vec::new();
    for (name, settings) in &cfg.volumes {
        let vstart = Instant::now();
        volumes.push(backup_vol(&ssh, &root_vol, name, settings, progress));
        info!(
            "Processing volume [cfg]{}/{}[/] took [result]{}[/]",
            root_vol,
            name,
            format_duration(vstart.elapsed())
        );
    }

    crate::cache::refresh(&cfg.name, &root_vol);

    BackupReport {
        config_file: path.display().to_string(),
        profile: Some(cfg.name),
        error: None,
        volumes,
        duration: start.elapsed(),
    }
}

pub(crate) fn config_files(dir: &Path) -> Result<Vec<PathBuf>> {
    info!("Scanning config files");

    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        // A missing config directory just means there is nothing configured yet.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            debug!("Config directory [cfg]{}[/] does not exist", dir.display());

            return Ok(Vec::new());
        }
        Err(e) => {
            return Err(e).context(format!(
                "Unable to scan config directory [cfg]{}[/]",
                dir.display()
            ));
        }
    };

    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("yml"))
        .collect();
    files.sort();

    Ok(files)
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let s = secs % 60;

    let mut out = String::new();
    if days > 0 {
        out.push_str(&format!("{}[sep]d[/]", days));
    }
    if hours > 0 {
        out.push_str(&format!("{}[sep]h[/]", hours));
    }
    if mins > 0 {
        out.push_str(&format!("{}[sep]m[/]", mins));
    }
    out.push_str(&format!("{}[sep]s[/]", s));

    out
}

/// A profile is fully successful when the config loaded and every volume
/// succeeded. No volumes is success: there was nothing to fail.
fn profile_succeeded(report: &BackupReport) -> bool {
    report.error.is_none() && report.volumes.iter().all(|v| v.status == Status::Ok)
}

pub fn run(
    config_directory: &Path,
    targets: &[String],
    mail_report: Option<&str>,
    errors_only: bool,
    cleanup: bool,
) -> Result<()> {
    let started = Local::now();
    let start_all = Instant::now();

    // Load and filter first, so the progress bar total matches the work we
    // actually do and we never load a config twice.
    let mut jobs: Vec<(PathBuf, Config)> = Vec::new();
    let mut backups: Vec<BackupReport> = Vec::new();
    for path in config_files(config_directory)? {
        match Config::load(&path) {
            Ok(cfg) => {
                if !targets.is_empty() && !targets.contains(&cfg.name) {
                    continue;
                }
                jobs.push((path, cfg));
            }
            Err(e) => {
                error!("{:#}", e);
                error!("Skipping backup related to [cfg]{}[/]", path.display());

                // When targeting specific backups we can't match an unloadable
                // config's name, so it stays out of the report (still logged).
                if targets.is_empty() {
                    backups.push(BackupReport {
                        config_file: path.display().to_string(),
                        profile: None,
                        error: Some(crate::logger::render_plain(&format!("{e:#}"))),
                        volumes: Vec::new(),
                        duration: Duration::default(),
                    });
                }
            }
        }
    }

    let mut progress = BackupProgress::new(jobs.len());
    for (path, cfg) in jobs {
        let start = Instant::now();
        progress.set_outer(
            &path
                .file_name()
                .unwrap_or(path.as_os_str())
                .to_string_lossy(),
        );
        let report = backup(&path, cfg, &mut progress);
        if cleanup {
            match (&report.profile, profile_succeeded(&report)) {
                (Some(name), true) => {
                    // cleanup draws its own bar; two live displays fight for the terminal.
                    progress.pause();
                    let result = crate::cleanup::run(config_directory, Some(name), false, false);
                    progress.resume();
                    result?;
                }
                (Some(name), false) => {
                    info!("Skipping cleanup for [cfg]{}[/]: backup failed", name);
                }
                (None, _) => {}
            }
        }
        backups.push(report);
        progress.advance_outer();
        info!(
            "Processing [cfg]{}[/] took [result]{}[/]",
            path.display(),
            format_duration(start.elapsed())
        );
    }
    progress.finish();

    info!(
        "Processing [cfg]all[/] took [result]{}[/]",
        format_duration(start_all.elapsed())
    );

    let report = RunReport {
        hostname: crate::report::hostname(),
        started,
        duration: start_all.elapsed(),
        backups,
    };

    if let Some(to) = mail_report {
        if errors_only && !report.has_errors() {
            info!("No errors: skipping mail report ([cfg]--errors-only[/])");
        } else if let Err(e) = crate::mailer::send(&report, to) {
            error!("Failed to send mail report: {:#}", e);
        } else {
            info!("Sent backup report to [cfg]{}[/]", to);
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_percent_reads_rsync_progress2_lines() {
        assert_eq!(
            parse_percent(b"  1,234,567  45%    1.23MB/s    0:00:12"),
            Some(45)
        );
        assert_eq!(parse_percent(b" 0   0%    0.00kB/s    0:00:00"), Some(0));
        assert_eq!(parse_percent(b"100%"), Some(100));
        assert_eq!(parse_percent(b"200%"), Some(100)); // clamped
        assert_eq!(parse_percent(b"no percent here"), None);
    }

    fn sample_report(
        profile: Option<&str>,
        error: Option<&str>,
        statuses: &[Status],
    ) -> BackupReport {
        BackupReport {
            config_file: "x.yml".to_string(),
            profile: profile.map(str::to_string),
            error: error.map(str::to_string),
            volumes: statuses
                .iter()
                .map(|status| VolumeReport {
                    name: "vol".to_string(),
                    source: "/src".to_string(),
                    status: *status,
                    error: None,
                    duration: Duration::default(),
                })
                .collect(),
            duration: Duration::default(),
        }
    }

    #[test]
    fn profile_succeeded_requires_every_volume_and_no_config_error() {
        assert!(profile_succeeded(&sample_report(
            Some("ok"),
            None,
            &[Status::Ok, Status::Ok]
        )));
        assert!(profile_succeeded(&sample_report(Some("empty"), None, &[])));
        assert!(!profile_succeeded(&sample_report(
            Some("bad"),
            None,
            &[Status::Ok, Status::Failed]
        )));
        assert!(!profile_succeeded(&sample_report(
            Some("cfg"),
            Some("yaml"),
            &[Status::Ok]
        )));
    }

    #[test]
    fn format_duration_omits_zero_leading_units_but_always_shows_seconds() {
        assert_eq!(format_duration(Duration::from_secs(0)), "0[sep]s[/]");
        assert_eq!(format_duration(Duration::from_secs(45)), "45[sep]s[/]");
        assert_eq!(
            format_duration(Duration::from_secs(90)),
            "1[sep]m[/]30[sep]s[/]"
        );
        assert_eq!(
            format_duration(Duration::from_secs(3_661)),
            "1[sep]h[/]1[sep]m[/]1[sep]s[/]"
        );
        assert_eq!(
            format_duration(Duration::from_secs(90_061)),
            "1[sep]d[/]1[sep]h[/]1[sep]m[/]1[sep]s[/]"
        );
    }
}
