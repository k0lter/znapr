use std::path::Path;

use anyhow::Result;
use chrono::Local;
use tracing::{debug, error, info};

use crate::backup::{call, config_files};
use crate::cache::{self, humanize_bytes, snapshot_expiry};
use crate::config::Config;
use crate::logger::render_markup;

fn pln(markup: &str) {
    println!("{}", render_markup(markup));
}

pub fn print_summary(line: &str) {
    pln(line);
}

pub fn run(
    config_directory: &Path,
    target: Option<&str>,
    dry_run: bool,
    no_cache: bool,
) -> Result<String> {
    let now = Local::now().naive_local();
    let verb = if dry_run { "would free" } else { "freed" };
    let mut total_freed: u64 = 0;
    let mut total_count: usize = 0;

    // Collect matching profiles first so the progress bar total is accurate.
    let mut jobs: Vec<(String, String)> = Vec::new();
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

        jobs.push((cfg.name.clone(), format!("{}/{}", cfg.zfsroot, cfg.name)));
    }

    let mut bar = crate::progress::Bar::new("", jobs.len());

    for (name, rootvol) in &jobs {
        bar.set_message(name);

        // Default to the cache (fast); fall back to a live query if it's cold.
        // `--no-cache` forces a live query for up-to-date, safest data.
        let entry = if no_cache {
            cache::build(name, rootvol)
        } else {
            cache::load(name).unwrap_or_else(|| cache::build(name, rootvol))
        };

        let expired: Vec<_> = entry
            .snapshots
            .iter()
            .filter(|s| snapshot_expiry(&s.name).is_some_and(|expiry| expiry < now))
            .collect();

        if expired.is_empty() {
            bar.inc();

            continue;
        }

        let mut destroyed_any = false;
        let mut freed: u64 = 0;
        let mut count: usize = 0;
        for s in expired {
            let size = humanize_bytes(s.used_bytes);

            if dry_run {
                debug!(
                    "[skip]would destroy[/] [cfg]{}[/] ([data]{}[/])",
                    s.name, size
                );
                freed += s.used_bytes;
                count += 1;
            } else {
                let r = call(&["zfs", "destroy", &s.name], true);

                if r.code == 0 {
                    debug!("[ok]destroyed[/] [cfg]{}[/] ([data]{}[/])", s.name, size);
                    freed += s.used_bytes;
                    count += 1;
                    destroyed_any = true;
                } else {
                    error!(
                        "Failed to destroy [data]{}[/]: [err]{}[/]",
                        s.name, r.stderr
                    );
                }
            }
        }

        if count > 0 {
            info!(
                "[cfg]{}[/]: [result]{}[/] [data]{}[/] across [result]{}[/] snapshots",
                name,
                verb,
                humanize_bytes(freed),
                count
            );
        }

        total_freed += freed;
        total_count += count;

        if destroyed_any {
            cache::refresh(name, rootvol);
        }

        bar.inc();
    }
    bar.finish();

    // Freed space is the sum of each snapshot's `used`; blocks shared between
    // snapshots make this an estimate. Upgrade path: `zfs destroy -nv` reports
    // exact reclaim for a set.
    // The caller prints this. `backup -C` must wait until its bar has stopped:
    // a line written while the bar will redraw is overwritten by cursor moves.
    Ok(format!(
        "[result]total {}[/] [data]{}[/] across [result]{}[/] snapshots",
        verb,
        humanize_bytes(total_freed),
        total_count
    ))
}
