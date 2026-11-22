use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::Local;
use tracing::error;

use crate::backup::{call, config_files};
use crate::cache::{self, snapshot_expiry};
use crate::config::Config;
use crate::list::{SortBy, append_total, print_table, row_from_cache, rows_json, sort_rows};

/// Snapshot names under `root`. A root `zfs list` rejects yields nothing.
fn snapshots_under(root: &str) -> Vec<String> {
    let r = call(
        &[
            "zfs", "list", "-H", "-t", "snapshot", "-r", "-o", "name", root,
        ],
        true,
    );

    if r.code != 0 {
        return Vec::new();
    }

    r.stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// All pool names, used as default roots when none is given.
fn pools() -> Vec<String> {
    let r = call(&["zpool", "list", "-H", "-o", "name"], true);

    if r.code != 0 {
        return Vec::new();
    }

    r.stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// Znapr snapshots (`dataset@YYYY-MM-DD_HH.MM.SS--Nd`) whose dataset is not a
/// configured volume. The same name is kept once, then sorted.
fn orphan_snapshots<'a>(
    names: impl IntoIterator<Item = &'a str>,
    known: &HashSet<String>,
) -> Vec<String> {
    let mut found = HashSet::new();
    for name in names {
        let Some(dataset) = name.split_once('@').map(|(dataset, _)| dataset) else {
            continue;
        };
        if snapshot_expiry(name).is_none() || known.contains(dataset) {
            continue;
        }
        found.insert(name.to_string());
    }

    let mut out: Vec<_> = found.into_iter().collect();
    out.sort();
    out
}

/// Datasets that hold an orphan snapshot. One dataset, however many snapshots.
fn orphan_datasets<'a>(
    names: impl IntoIterator<Item = &'a str>,
    known: &HashSet<String>,
) -> Vec<String> {
    let mut datasets = HashSet::new();
    for name in orphan_snapshots(names, known) {
        if let Some((dataset, _)) = name.split_once('@') {
            datasets.insert(dataset.to_string());
        }
    }

    let mut out: Vec<_> = datasets.into_iter().collect();
    out.sort();
    out
}

pub fn run(
    config_directory: &Path,
    targets: &[String],
    total: bool,
    no_headers: bool,
    json: bool,
) -> Result<()> {
    let mut known: HashSet<String> = HashSet::new();
    for path in config_files(config_directory)? {
        match Config::load(path.as_path()) {
            Ok(cfg) => {
                for volume in cfg.volumes.keys() {
                    known.insert(format!("{}/{}/{}", cfg.zfsroot, cfg.name, volume));
                }
            }
            Err(e) => error!("{:#}", e),
        }
    }

    let roots = if targets.is_empty() {
        pools()
    } else {
        targets.to_vec()
    };

    let names: Vec<String> = roots
        .iter()
        .flat_map(|root| snapshots_under(root))
        .collect();
    let datasets = orphan_datasets(names.iter().map(String::as_str), &known);

    // Live ZFS query. An orphan has no profile, so nothing is read from or
    // written to /var/cache/znapr.
    let now = Local::now().naive_local();
    let mut pool_sizes: HashMap<String, Option<u64>> = HashMap::new();
    let mut rows = Vec::new();
    for dataset in datasets {
        let pool = cache::pool_of(&dataset).to_string();
        let size = *pool_sizes
            .entry(pool.clone())
            .or_insert_with(|| cache::pool_size(&pool));
        rows.push(row_from_cache(
            &dataset,
            &cache::build(&dataset, &dataset),
            now,
            &pool,
            size,
            &[],
        ));
    }

    sort_rows(&mut rows, SortBy::Name, false);
    if total {
        append_total(&mut rows);
    }
    if json {
        println!("{}", rows_json(&rows));
    } else {
        print_table("DATASET", &rows, !no_headers);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orphan_snapshots_keeps_znapr_names_outside_known_volumes() {
        let known = HashSet::from(["tank/backup/mail/spool".to_string()]);
        let names = [
            "tank/backup/mail/spool@2026-11-22_09.54.52--15d",
            "tank/backup/old/spool@2026-11-22_09.54.52--15d",
            "tank/backup/old/spool@2026-11-21_09.54.52--15d",
            "tank/other@2026-11-22_09.54.52--7d",
            "tank/backup/old/spool@autosnap",
            "tank/backup/old/spool@2026-11-22_09.54.52--15",
        ];

        assert_eq!(
            orphan_snapshots(names, &known),
            vec![
                "tank/backup/old/spool@2026-11-21_09.54.52--15d".to_string(),
                "tank/backup/old/spool@2026-11-22_09.54.52--15d".to_string(),
                "tank/other@2026-11-22_09.54.52--7d".to_string(),
            ]
        );
        assert_eq!(
            orphan_datasets(names, &known),
            vec![
                "tank/backup/old/spool".to_string(),
                "tank/other".to_string(),
            ]
        );
    }
}
