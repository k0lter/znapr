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
        #[arg(short = 'M', long = "mail-report-to", value_name = "EMAIL")]
        mail_report_to: Option<String>,

        /// Sender address of the report. Default is `znapr@` plus `hostname -f`
        #[arg(
            short = 'F',
            long = "mail-report-from",
            value_name = "EMAIL",
            requires = "mail_report_to"
        )]
        mail_report_from: Option<String>,

        /// Only send the report if a backup failed (requires --mail-report-to)
        #[arg(short = 'E', long = "errors-only", requires = "mail_report_to")]
        errors_only: bool,

        /// Destroy expired snapshots of each fully successful profile (same as `cleanup`)
        #[arg(short = 'C', long = "cleanup")]
        cleanup: bool,

        /// One line per volume, with its source and duration. The default report is one line per profile.
        #[arg(short = 'X', long = "extended-report", requires = "mail_report_to")]
        extended_report: bool,
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

        /// Add a final row with the sums
        #[arg(short = 'T', long = "total")]
        total: bool,

        /// Do not print the column header line
        #[arg(short = 'H', long = "no-headers")]
        no_headers: bool,

        /// Print the rows as a JSON array
        #[arg(short = 'J', long = "json")]
        json: bool,
    },
    /// List znapr snapshots that belong to no configured volume
    Orphaned {
        /// Configuration directory
        #[arg(short = 'c', long = "config-directory", default_value = "/etc/znapr")]
        config_directory: PathBuf,

        /// Pools or datasets to scan (default: every pool from `zpool list`)
        #[arg(value_name = "POOL_OR_DATASET")]
        targets: Vec<String>,

        /// Add a final row with the sums
        #[arg(short = 'T', long = "total")]
        total: bool,

        /// Do not print the column header line
        #[arg(short = 'H', long = "no-headers")]
        no_headers: bool,

        /// Print the rows as a JSON array
        #[arg(short = 'J', long = "json")]
        json: bool,
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
            mail_report_to,
            mail_report_from,
            errors_only,
            cleanup,
            extended_report,
        } => backup::run(
            &config_directory,
            &targets,
            mail_report_to.as_deref(),
            mail_report_from.as_deref(),
            errors_only,
            cleanup,
            extended_report,
        )?,
        Commands::List {
            config_directory,
            target,
            sort_size,
            sort_last,
            reverse,
            total,
            no_headers,
            json,
        } => list::run(
            &config_directory,
            target.as_deref(),
            sort_size,
            sort_last,
            reverse,
            list::Output {
                total,
                no_headers,
                json,
            },
        )?,
        Commands::Orphaned {
            config_directory,
            targets,
            total,
            no_headers,
            json,
        } => orphaned::run(&config_directory, &targets, total, no_headers, json)?,
        Commands::Recache {
            config_directory,
            target,
        } => cache::recache(&config_directory, target.as_deref())?,
        Commands::Cleanup {
            config_directory,
            target,
            dry_run,
            no_cache,
        } => {
            let summary = cleanup::run(&config_directory, target.as_deref(), dry_run, no_cache)?;
            cleanup::print_summary(&summary);
        }
    }

    Ok(())
}

fn main() {
    if let Err(e) = run() {
        error!("{:#}", e);
        process::exit(1);
    }
}
