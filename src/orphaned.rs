use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Result;
use chrono::Local;
use tracing::error;

use crate::backup::{call, config_files};
use crate::cache;
use crate::config::Config;
use crate::list::{Row, SortBy, print_table, row_from_cache, sort_rows};

/// Immediate child datasets of a zfsroot (the per-backup roots), excluding the
/// root itself.
fn children(zfsroot: &str) -> Vec<String> {
    let r = call(
        &["zfs", "list", "-H", "-r", "-d", "1", "-o", "name", zfsroot],
        true,
    );

    if r.code != 0 {
        return Vec::new();
    }

    r.stdout
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && *l != zfsroot)
        .map(String::from)
        .collect()
}

/// All pool names, used as default zfsroots to scan when none is given.
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

pub fn run(config_directory: &Path, zfsroot: Option<&str>) -> Result<()> {
    let now = Local::now().naive_local();

    let mut known: HashSet<String> = HashSet::new();
    for path in config_files(config_directory)? {
        match Config::load(path.as_path()) {
            Ok(cfg) => {
                known.insert(format!("{}/{}", cfg.zfsroot, cfg.name));
            }
            Err(e) => error!("{:#}", e),
        }
    }

    let roots = match zfsroot {
        Some(z) => vec![z.to_string()],
        None => pools(),
    };

    let mut pool_sizes: HashMap<String, Option<u64>> = HashMap::new();
    let mut rows: Vec<Row> = roots
        .iter()
        .flat_map(|root| children(root))
        .filter(|child| !known.contains(child))
        // Orphans have no YAML, so no cache: always query zfs live (not persisted).
        .map(|child| {
            let pool = cache::pool_of(&child).to_string();
            let size = *pool_sizes
                .entry(pool.clone())
                .or_insert_with(|| cache::pool_size(&pool));

            // Orphans have no config, so "fully successful" is undefined (shown as "-").
            row_from_cache(&child, &cache::build(&child, &child), now, size, &[])
        })
        .collect();

    sort_rows(&mut rows, SortBy::Name, false);
    print_table("DATASET", &rows);

    Ok(())
}
