use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono::{Local, NaiveDateTime};
use tracing::error;

use crate::backup::config_files;
use crate::cache::{self, BackupCache, humanize_bytes};
use crate::config::Config;
use crate::logger::render_stdout;

pub(crate) enum SortBy {
    Name,
    Size,
    Last,
}

pub(crate) struct Row {
    name: String,
    snapshots: String,
    expired: String,
    used: String,
    pool_pct: String,
    last_ok: String,
    used_bytes: u64,
    last_ok_date: Option<NaiveDateTime>,
    has_expired: bool,
    snapshot_count: usize,
    expired_count: usize,
    pool: String,
    pool_bytes: Option<u64>,
}

pub(crate) fn row_from_cache(
    display_name: &str,
    cache: &BackupCache,
    now: NaiveDateTime,
    pool: &str,
    pool_size: Option<u64>,
    volumes: &[String],
) -> Row {
    let pool_bytes = pool_size.filter(|size| *size > 0);

    if !cache.exists {
        return Row {
            name: display_name.to_string(),
            snapshots: "-".to_string(),
            expired: "-".to_string(),
            used: "-".to_string(),
            pool_pct: "-".to_string(),
            last_ok: "-".to_string(),
            used_bytes: 0,
            last_ok_date: None,
            has_expired: false,
            snapshot_count: 0,
            expired_count: 0,
            pool: pool.to_string(),
            pool_bytes,
        };
    }

    let expired = cache.expired_count(now);
    let pool_pct = match pool_bytes {
        Some(size) => format!("{:.1}%", cache.used_bytes as f64 / size as f64 * 100.0),
        None => "-".to_string(),
    };

    let last_ok_date = cache.last_full_success(volumes);
    let last_ok = last_ok_date
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_else(|| "-".to_string());

    Row {
        name: display_name.to_string(),
        snapshots: cache.snapshots.len().to_string(),
        expired: expired.to_string(),
        used: humanize_bytes(cache.used_bytes),
        pool_pct,
        last_ok,
        used_bytes: cache.used_bytes,
        last_ok_date,
        has_expired: expired > 0,
        snapshot_count: cache.snapshots.len(),
        expired_count: expired,
        pool: pool.to_string(),
        pool_bytes,
    }
}

/// One trailing row: sums of snapshots, expired, and used space. The percent
/// is total used over the sum of distinct pool sizes. A missing pool size
/// makes that cell `-`.
pub(crate) fn append_total(rows: &mut Vec<Row>) {
    let snapshots: usize = rows.iter().map(|r| r.snapshot_count).sum();
    let expired: usize = rows.iter().map(|r| r.expired_count).sum();
    let used_bytes: u64 = rows.iter().map(|r| r.used_bytes).sum();

    let mut pools: HashMap<&str, u64> = HashMap::new();
    let mut missing_pool = false;
    for row in rows.iter() {
        match row.pool_bytes {
            Some(size) => {
                pools.insert(row.pool.as_str(), size);
            }
            None => missing_pool = true,
        }
    }
    let pool_pct = match pools.values().copied().sum::<u64>() {
        denom if !missing_pool && denom > 0 => {
            format!("{:.1}%", used_bytes as f64 / denom as f64 * 100.0)
        }
        _ => "-".to_string(),
    };

    rows.push(Row {
        name: "TOTAL".to_string(),
        snapshots: snapshots.to_string(),
        expired: expired.to_string(),
        used: humanize_bytes(used_bytes),
        pool_pct,
        last_ok: "-".to_string(),
        used_bytes,
        last_ok_date: None,
        has_expired: expired > 0,
        snapshot_count: snapshots,
        expired_count: expired,
        pool: String::new(),
        pool_bytes: None,
    });
}

pub(crate) fn sort_rows(rows: &mut [Row], by: SortBy, reverse: bool) {
    match by {
        SortBy::Name => rows.sort_by(|a, b| a.name.cmp(&b.name)),
        // Largest / most recent first (the common intent). Rows with no date
        // (never fully backed up) sort last.
        SortBy::Size => rows.sort_by(|a, b| b.used_bytes.cmp(&a.used_bytes)),
        SortBy::Last => rows.sort_by(|a, b| b.last_ok_date.cmp(&a.last_ok_date)),
    }

    if reverse {
        rows.reverse();
    }
}

fn pad_left(s: &str, w: usize) -> String {
    format!("{:<w$}", s)
}

fn pad_right(s: &str, w: usize) -> String {
    format!("{:>w$}", s)
}

/// Indented JSON array of the table rows. A `-` cell is `null`.
pub(crate) fn rows_json(rows: &[Row]) -> String {
    #[derive(serde::Serialize)]
    struct JsonRow<'a> {
        name: &'a str,
        snapshots: Option<usize>,
        expired: Option<usize>,
        used_bytes: Option<u64>,
        pool_percent: Option<f64>,
        last_ok: Option<&'a str>,
    }

    let items: Vec<JsonRow> = rows
        .iter()
        .map(|row| JsonRow {
            name: &row.name,
            snapshots: count_cell(&row.snapshots, row.snapshot_count),
            expired: count_cell(&row.expired, row.expired_count),
            used_bytes: if row.used == "-" {
                None
            } else {
                Some(row.used_bytes)
            },
            pool_percent: row.pool_pct.strip_suffix('%').and_then(|n| n.parse().ok()),
            last_ok: if row.last_ok == "-" {
                None
            } else {
                Some(row.last_ok.as_str())
            },
        })
        .collect();

    serde_json::to_string_pretty(&items).expect("row json")
}

fn count_cell(display: &str, count: usize) -> Option<usize> {
    if display == "-" { None } else { Some(count) }
}

pub(crate) fn print_table(name_header: &str, rows: &[Row], show_headers: bool) {
    let headers = [
        name_header,
        "SNAPSHOTS",
        "EXPIRED",
        "USED",
        "POOL%",
        "LAST OK",
    ];

    let mut w = headers.map(str::len);
    for r in rows {
        w[0] = w[0].max(r.name.len());
        w[1] = w[1].max(r.snapshots.len());
        w[2] = w[2].max(r.expired.len());
        w[3] = w[3].max(r.used.len());
        w[4] = w[4].max(r.pool_pct.len());
        w[5] = w[5].max(r.last_ok.len());
    }

    let header = [
        pad_left(headers[0], w[0]),
        pad_right(headers[1], w[1]),
        pad_right(headers[2], w[2]),
        pad_right(headers[3], w[3]),
        pad_right(headers[4], w[4]),
        pad_right(headers[5], w[5]),
    ]
    .join("  ");
    if show_headers {
        println!("{}", render_stdout(&header));
    }

    for r in rows {
        let name = pad_left(&r.name, w[0]);
        let snaps = pad_right(&r.snapshots, w[1]);
        let expired = pad_right(&r.expired, w[2]);
        let used = pad_right(&r.used, w[3]);
        let pool_pct = pad_right(&r.pool_pct, w[4]);
        let last_ok = pad_right(&r.last_ok, w[5]);
        let expired_tag = if r.has_expired { "err" } else { "sep" };

        let line = format!(
            "[cfg]{name}[/]  [data]{snaps}[/]  [{expired_tag}]{expired}[/]  [data]{used}[/]  [sep]{pool_pct}[/]  [sep]{last_ok}[/]"
        );
        println!("{}", render_stdout(&line));
    }
}

pub fn run(
    config_directory: &Path,
    target: Option<&str>,
    sort_size: bool,
    sort_last: bool,
    reverse: bool,
    total: bool,
    no_headers: bool,
    json: bool,
) -> Result<()> {
    let now = Local::now().naive_local();
    let sort_by = if sort_size {
        SortBy::Size
    } else if sort_last {
        SortBy::Last
    } else {
        SortBy::Name
    };

    // Gather profiles and their cache state first, so the progress bar only
    // appears when some entries are cold (a warm cache stays instant).
    let mut items: Vec<(String, String, Vec<String>, Option<BackupCache>)> = Vec::new();
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

        let rootvol = format!("{}/{}", cfg.zfsroot, cfg.name);
        let volumes: Vec<String> = cfg.volumes.keys().cloned().collect();
        let cached = cache::load(&cfg.name);
        items.push((cfg.name, rootvol, volumes, cached));
    }

    let cold = items.iter().filter(|(_, _, _, c)| c.is_none()).count();
    let mut bar = crate::progress::Bar::new("", cold);

    let mut pool_sizes: HashMap<String, Option<u64>> = HashMap::new();
    let mut rows: Vec<Row> = Vec::new();

    for (name, rootvol, volumes, cached) in items {
        // Cache miss: query zfs live and warm the cache for next time.
        let entry = match cached {
            Some(c) => c,
            None => {
                bar.set_message(&name);
                let fresh = cache::build(&name, &rootvol);
                cache::refresh(&name, &rootvol);
                bar.inc();

                fresh
            }
        };

        let pool = cache::pool_of(&rootvol).to_string();
        let size = *pool_sizes
            .entry(pool.clone())
            .or_insert_with(|| cache::pool_size(&pool));

        rows.push(row_from_cache(&name, &entry, now, &pool, size, &volumes));
    }
    bar.finish();

    sort_rows(&mut rows, sort_by, reverse);
    if total {
        append_total(&mut rows);
    }
    if json {
        println!("{}", rows_json(&rows));
    } else {
        print_table("NAME", &rows, !no_headers);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        name: &str,
        snaps: usize,
        expired: usize,
        used: u64,
        pool: &str,
        pool_bytes: Option<u64>,
    ) -> Row {
        Row {
            name: name.to_string(),
            snapshots: snaps.to_string(),
            expired: expired.to_string(),
            used: humanize_bytes(used),
            pool_pct: "-".to_string(),
            last_ok: "-".to_string(),
            used_bytes: used,
            last_ok_date: None,
            has_expired: expired > 0,
            snapshot_count: snaps,
            expired_count: expired,
            pool: pool.to_string(),
            pool_bytes,
        }
    }

    #[test]
    fn append_total_sums_counts_and_distinct_pools() {
        let mut rows = vec![
            row("a", 2, 1, 10, "tank", Some(100)),
            row("b", 3, 0, 10, "tank", Some(100)),
            row("c", 1, 0, 20, "rpool", Some(200)),
        ];
        append_total(&mut rows);

        let total = rows.last().unwrap();
        assert_eq!(total.name, "TOTAL");
        assert_eq!(total.snapshots, "6");
        assert_eq!(total.expired, "1");
        assert_eq!(total.used, "40B");
        assert_eq!(total.pool_pct, "13.3%");
        assert_eq!(total.last_ok, "-");
        assert!(total.has_expired);

        let mut missing = vec![
            row("a", 1, 0, 10, "tank", Some(100)),
            row("b", 1, 0, 10, "rpool", None),
        ];
        append_total(&mut missing);
        assert_eq!(missing.last().unwrap().pool_pct, "-");
    }

    #[test]
    fn rows_json_uses_null_for_missing_cells() {
        let mut present = row("mail", 2, 1, 10, "tank", Some(100));
        present.pool_pct = "1.5%".to_string();
        present.last_ok = "2026-11-22".to_string();
        let missing = Row {
            name: "gone".to_string(),
            snapshots: "-".to_string(),
            expired: "-".to_string(),
            used: "-".to_string(),
            pool_pct: "-".to_string(),
            last_ok: "-".to_string(),
            used_bytes: 0,
            last_ok_date: None,
            has_expired: false,
            snapshot_count: 0,
            expired_count: 0,
            pool: "tank".to_string(),
            pool_bytes: Some(100),
        };

        let value: serde_json::Value =
            serde_json::from_str(&rows_json(&[present, missing])).unwrap();
        assert_eq!(value[0]["name"], "mail");
        assert_eq!(value[0]["snapshots"], 2);
        assert_eq!(value[0]["expired"], 1);
        assert_eq!(value[0]["used_bytes"], 10);
        assert_eq!(value[0]["pool_percent"], 1.5);
        assert_eq!(value[0]["last_ok"], "2026-11-22");
        assert!(value[1]["snapshots"].is_null());
        assert!(value[1]["expired"].is_null());
        assert!(value[1]["used_bytes"].is_null());
        assert!(value[1]["pool_percent"].is_null());
        assert!(value[1]["last_ok"].is_null());
    }
}
