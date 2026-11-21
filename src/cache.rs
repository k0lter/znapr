use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{Local, NaiveDateTime};
use serde::{Deserialize, Serialize};

use tracing::{debug, error, info};

use crate::backup::{call, config_files};
use crate::config::Config;

const CACHE_DIR: &str = "/var/cache/znapr";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotInfo {
    pub name: String,
    pub used_bytes: u64,
}

/// Cached state of a single backup (one YAML profile), keyed by profile name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupCache {
    pub name: String,
    pub rootvol: String,
    // Whether the root dataset exists yet (a configured backup may never have run).
    pub exists: bool,
    pub used_bytes: u64,
    pub snapshots: Vec<SnapshotInfo>,
    pub updated_at: String,
}

fn cache_dir() -> PathBuf {
    // ZNAPR_CACHE_DIR override keeps the cache testable and relocatable.
    std::env::var_os("ZNAPR_CACHE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(CACHE_DIR))
}

fn cache_path(name: &str) -> PathBuf {
    cache_dir().join(format!("{name}.json"))
}

/// Snapshot names and their individual `used` size in bytes, in one zfs call.
/// `None` means the root dataset does not exist.
fn snapshots_with_size(rootvol: &str) -> Option<Vec<SnapshotInfo>> {
    let r = call(
        &[
            "zfs", "list", "-H", "-p", "-t", "snapshot", "-r", "-o", "name,used", rootvol,
        ],
        true,
    );

    if r.code != 0 {
        return None;
    }

    Some(
        r.stdout
            .lines()
            .filter_map(|l| {
                let (name, used) = l.split_once('\t')?;
                let name = name.trim();

                if name.is_empty() {
                    return None;
                }

                Some(SnapshotInfo {
                    name: name.to_string(),
                    used_bytes: used.trim().parse().unwrap_or(0),
                })
            })
            .collect(),
    )
}

fn volume_used(rootvol: &str) -> u64 {
    let r = call(&["zfs", "list", "-H", "-p", "-o", "used", rootvol], true);

    if r.code == 0 {
        r.stdout.trim().parse().unwrap_or(0)
    } else {
        0
    }
}

/// Query ZFS live and assemble a fresh cache entry (not persisted).
pub fn build(name: &str, rootvol: &str) -> BackupCache {
    let updated_at = Local::now().to_rfc3339();

    match snapshots_with_size(rootvol) {
        Some(snapshots) => BackupCache {
            name: name.to_string(),
            rootvol: rootvol.to_string(),
            exists: true,
            used_bytes: volume_used(rootvol),
            snapshots,
            updated_at,
        },
        None => BackupCache {
            name: name.to_string(),
            rootvol: rootvol.to_string(),
            exists: false,
            used_bytes: 0,
            snapshots: Vec::new(),
            updated_at,
        },
    }
}

pub fn load(name: &str) -> Option<BackupCache> {
    let content = fs::read_to_string(cache_path(name)).ok()?;

    serde_json::from_str(&content).ok()
}

pub fn save(cache: &BackupCache) -> Result<()> {
    let dir = cache_dir();
    fs::create_dir_all(&dir).context(format!(
        "Failed to create cache directory [path]{}[/]",
        dir.display()
    ))?;

    let path = cache_path(&cache.name);
    let json = serde_json::to_string_pretty(cache).context("Failed to serialize cache")?;
    fs::write(&path, json).context(format!("Failed to write cache [path]{}[/]", path.display()))?;
    debug!("Updated cache [path]{}[/]", path.display());

    Ok(())
}

/// Rebuild the cache live and persist it, logging failures without aborting.
pub fn refresh(name: &str, rootvol: &str) {
    if let Err(e) = save(&build(name, rootvol)) {
        tracing::warn!("Could not update cache for [cfg]{}[/]: {}", name, e);
    }
}

/// Creation instant encoded in a snapshot name `dataset@YYYY-MM-DD_HH.MM.SS--Nd`.
/// Returns `None` for snapshots not created by znapr (unparseable).
pub fn snapshot_created(snapname: &str) -> Option<NaiveDateTime> {
    let tag = snapname.split_once('@')?.1;
    let (date, _days) = tag.rsplit_once("--")?;

    NaiveDateTime::parse_from_str(date, "%Y-%m-%d_%H.%M.%S").ok()
}

/// Expiry instant of a snapshot: its creation date plus the retention (`Nd`)
/// encoded in its name. `None` for snapshots not created by znapr.
pub fn snapshot_expiry(snapname: &str) -> Option<NaiveDateTime> {
    let days = snapname.rsplit_once("--")?.1.strip_suffix('d')?;
    let days: i64 = days.parse().ok()?;

    Some(snapshot_created(snapname)? + chrono::Duration::days(days))
}

impl BackupCache {
    pub fn expired_count(&self, now: NaiveDateTime) -> usize {
        self.snapshots
            .iter()
            .filter(|s| snapshot_expiry(&s.name).is_some_and(|expiry| expiry < now))
            .count()
    }

    /// Date of the last backup where *every* configured volume had a snapshot:
    /// the minimum, across volumes, of each volume's most recent snapshot date.
    /// `None` if any configured volume has never been snapshotted successfully.
    pub fn last_full_success(&self, volumes: &[String]) -> Option<NaiveDateTime> {
        if volumes.is_empty() {
            return None;
        }

        let per_volume: Option<Vec<NaiveDateTime>> = volumes
            .iter()
            .map(|vol| {
                let dataset = format!("{}/{}", self.rootvol, vol);
                self.snapshots
                    .iter()
                    .filter(|s| s.name.split_once('@').map(|(d, _)| d) == Some(dataset.as_str()))
                    .filter_map(|s| snapshot_created(&s.name))
                    .max()
            })
            .collect();

        per_volume?.into_iter().min()
    }
}

/// Format a byte count the ZFS way: base-1024 units with adaptive precision.
pub fn humanize_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["B", "K", "M", "G", "T", "P"];

    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{}{}", bytes, UNITS[0])
    } else if value >= 100.0 {
        format!("{:.0}{}", value, UNITS[unit])
    } else if value >= 10.0 {
        format!("{:.1}{}", value, UNITS[unit])
    } else {
        format!("{:.2}{}", value, UNITS[unit])
    }
}

/// The pool a dataset belongs to (its first path component).
pub fn pool_of(dataset: &str) -> &str {
    dataset.split('/').next().unwrap_or(dataset)
}

pub fn pool_size(pool: &str) -> Option<u64> {
    let r = call(&["zpool", "list", "-H", "-p", "-o", "size", pool], true);

    if r.code == 0 {
        r.stdout.trim().parse().ok()
    } else {
        None
    }
}

pub fn recache(config_directory: &Path, target: Option<&str>) -> Result<()> {
    let mut targets: Vec<(String, String)> = Vec::new();
    for path in config_files(config_directory)? {
        let cfg = match Config::load(path.as_path()) {
            Ok(c) => c,
            Err(e) => {
                error!("{:#}", e);

                continue;
            }
        };

        if target.is_some_and(|t| cfg.name != t) {
            continue;
        }

        targets.push((cfg.name.clone(), format!("{}/{}", cfg.zfsroot, cfg.name)));
    }

    let mut bar = crate::progress::Bar::new("", targets.len());
    for (name, rootvol) in &targets {
        bar.set_message(name);
        info!("Caching backup [cfg]{}[/]", name);
        save(&build(name, rootvol))?;
        bar.inc();
    }
    bar.finish();

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;

    fn snap(name: &str) -> SnapshotInfo {
        SnapshotInfo {
            name: name.to_string(),
            used_bytes: 1024,
        }
    }

    #[test]
    fn snapshot_expiry_parses_znapr_names_and_rejects_others() {
        let expiry = snapshot_expiry("tank/b@2026-01-01_00.00.00--30d").expect("parseable");
        assert_eq!(
            expiry,
            NaiveDate::from_ymd_opt(2026, 1, 31)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap()
        );
        assert!(snapshot_expiry("tank/b@manual-snapshot").is_none());
    }

    #[test]
    fn expired_count_only_flags_past_expiry() {
        let now = NaiveDate::from_ymd_opt(2026, 7, 11)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let cache = BackupCache {
            name: "b".to_string(),
            rootvol: "tank/b".to_string(),
            exists: true,
            used_bytes: 1 << 30,
            snapshots: vec![
                snap("tank/b@2026-07-10_23.00.00--30d"),
                snap("tank/b@2026-01-01_00.00.00--30d"),
                snap("tank/b@manual"),
            ],
            updated_at: String::new(),
        };
        assert_eq!(cache.expired_count(now), 1);
    }

    #[test]
    fn last_full_success_is_min_of_each_volumes_latest() {
        let cache = BackupCache {
            name: "web".to_string(),
            rootvol: "tank/b/web".to_string(),
            exists: true,
            used_bytes: 0,
            snapshots: vec![
                snap("tank/b/web/root@2026-07-10_02.00.00--30d"),
                snap("tank/b/web/root@2026-07-13_02.00.00--30d"),
                snap("tank/b/web/db@2026-07-11_02.00.00--30d"),
            ],
            updated_at: String::new(),
        };

        let vols = vec!["root".to_string(), "db".to_string()];
        // min(latest root = 07-13, latest db = 07-11) = 07-11.
        assert_eq!(
            cache.last_full_success(&vols),
            Some(
                NaiveDate::from_ymd_opt(2026, 7, 11)
                    .unwrap()
                    .and_hms_opt(2, 0, 0)
                    .unwrap()
            )
        );

        // A configured volume with no snapshot yet => no complete backup.
        let vols_missing = vec!["root".to_string(), "cache".to_string()];
        assert_eq!(cache.last_full_success(&vols_missing), None);
    }

    #[test]
    fn humanize_bytes_matches_zfs_style_units() {
        assert_eq!(humanize_bytes(0), "0B");
        assert_eq!(humanize_bytes(512), "512B");
        assert_eq!(humanize_bytes(1024), "1.00K");
        assert_eq!(humanize_bytes(1536), "1.50K");
        assert_eq!(humanize_bytes(10 * 1024 * 1024), "10.0M");
        assert_eq!(humanize_bytes(4 * 1024 * 1024 * 1024), "4.00G");
    }
}
