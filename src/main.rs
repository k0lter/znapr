#[macro_use]
mod logger;

mod backup;
mod cache;
mod cleanup;
mod config;
mod list;
mod mailer;
mod orphaned;
mod progress;
mod report;

use std::path::PathBuf;
use std::process;

use clap::builder::styling::{AnsiColor, Styles};
use clap::{Parser, Subcommand};

use tracing::error;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().bold())
    .usage(AnsiColor::Green.on_default().bold())
    .literal(AnsiColor::Cyan.on_default().bold())
    .placeholder(AnsiColor::Yellow.on_default());

#[derive(Parser)]
#[command(
    name = "znapr",
    version,
    about = "ZFS snapshot and rsync backup tool",
    styles = STYLES,
)]
struct Cli {
    /// Increase log verbosity (-v = info, -vv = debug, -vvv = trace)
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    verbose: u8,

    /// Suppress all output except errors
    #[arg(short, long, global = true)]
    quiet: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Trigger all backups
    Backup {
        /// Configuration directory
        #[arg(short = 'c', long = "config-directory", default_value = "/etc/znapr")]
        config_directory: PathBuf,

        /// Backup profile name(s) to run (default: all)
        targets: Vec<String>,

        /// Send a backup report e-mail to this address (via the local MTA)
        #[arg(short = 'M', long = "mail-report", value_name = "EMAIL")]
        mail_report: Option<String>,

        /// Only send the report if a backup failed (implies --mail-report)
        #[arg(short = 'E', long = "errors-only", requires = "mail_report")]
        errors_only: bool,
    },
    /// List configured backups with their snapshot count and used space
    List {
        /// Configuration directory
        #[arg(short = 'c', long = "config-directory", default_value = "/etc/znapr")]
        config_directory: PathBuf,

        /// Backup profile name to filter on
        target: Option<String>,

        /// Sort by used size (largest first) instead of by name
        #[arg(short = 'S', long = "sort-size", conflicts_with = "sort_last")]
        sort_size: bool,

        /// Sort by last fully-successful backup date (most recent first)
        #[arg(short = 'L', long = "sort-last")]
        sort_last: bool,

        /// Reverse the sort order
        #[arg(short = 'r', long)]
        reverse: bool,
    },
    /// List orphaned datasets (backup volumes with no config file)
    Orphaned {
        /// Configuration directory
        #[arg(short = 'c', long = "config-directory", default_value = "/etc/znapr")]
        config_directory: PathBuf,

        /// zfsroot to scan (default: every pool from `zpool list`)
        zfsroot: Option<String>,
    },
    /// Rebuild the per-backup cache used by `list`
    Recache {
        /// Configuration directory
        #[arg(short = 'c', long = "config-directory", default_value = "/etc/znapr")]
        config_directory: PathBuf,

        /// Backup profile name to recache (default: all)
        target: Option<String>,
    },
    /// Destroy expired snapshots (retention encoded in snapshot names)
    Cleanup {
        /// Configuration directory
        #[arg(short = 'c', long = "config-directory", default_value = "/etc/znapr")]
        config_directory: PathBuf,

        /// Backup profile name to clean (default: all)
        target: Option<String>,

        /// Show what would be destroyed without deleting anything
        #[arg(short = 'n', long = "dry-run")]
        dry_run: bool,

        /// Query ZFS live instead of using the cache
        #[arg(short = 'C', long = "no-cache")]
        no_cache: bool,
    },
}

fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();

    logger::init(cli.verbose, cli.quiet);

    match cli.command {
        Commands::Backup {
            config_directory,
            targets,
            mail_report,
            errors_only,
        } => backup::run(&config_directory, &targets, mail_report.as_deref(), errors_only)?,
        Commands::List {
            config_directory,
            target,
            sort_size,
            sort_last,
            reverse,
        } => list::run(
            &config_directory,
            target.as_deref(),
            sort_size,
            sort_last,
            reverse,
        )?,
        Commands::Orphaned {
            config_directory,
            zfsroot,
        } => orphaned::run(&config_directory, zfsroot.as_deref())?,
        Commands::Recache {
            config_directory,
            target,
        } => cache::recache(&config_directory, target.as_deref())?,
        Commands::Cleanup {
            config_directory,
            target,
            dry_run,
            no_cache,
        } => cleanup::run(&config_directory, target.as_deref(), dry_run, no_cache)?,
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        error!("{:#}", e);
        process::exit(1);
    }
}
