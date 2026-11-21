use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;
use chrono::{Local, NaiveDateTime};
use tracing::error;

use crate::backup::config_files;
use crate::cache::{self, BackupCache, humanize_bytes};
use crate::config::Config;
use crate::logger::render_markup;

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
}

pub(crate) fn row_from_cache(
    display_name: &str,
    cache: &BackupCache,
    now: NaiveDateTime,
    pool_size: Option<u64>,
    volumes: &[String],
) -> Row {
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
        };
    }

    let expired = cache.expired_count(now);
    let pool_pct = match pool_size {
        Some(size) if size > 0 => {
            format!("{:.1}%", cache.used_bytes as f64 / size as f64 * 100.0)
        }
        _ => "-".to_string(),
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
    }
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

pub(crate) fn print_table(name_header: &str, rows: &[Row]) {
    let headers = [name_header, "SNAPSHOTS", "EXPIRED", "USED", "POOL%", "LAST OK"];

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
    println!("{}", render_markup(&header));

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
        println!("{}", render_markup(&line));
    }
}

pub fn run(
    config_directory: &Path,
    target: Option<&str>,
    sort_size: bool,
    sort_last: bool,
    reverse: bool,
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

        rows.push(row_from_cache(&name, &entry, now, size, &volumes));
    }
    bar.finish();

    sort_rows(&mut rows, sort_by, reverse);
    print_table("NAME", &rows);

    Ok(())
}
