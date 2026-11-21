use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Deserialize;

use tracing::{info, warn};

/// What to do when rsync exits 24 (source files vanished mid-transfer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Vanished {
    /// Fail the volume and skip the snapshot. Today's behaviour, opt-in.
    Error,
    /// Keep the transfer, warn, and snapshot.
    Ignore,
    /// Keep the transfer and snapshot, with no log line.
    Silent,
    /// Re-run up to a small cap, then treat a last 24 like [`Ignore`](Self::Ignore).
    #[default]
    Retry,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Volume {
    pub source: String,
    #[serde(rename = "max-days")]
    pub max_days: i64,
    #[serde(default)]
    pub excludes: Option<Vec<String>>,
    /// Overrides the profile-level [`Config::vanished`] when set.
    #[serde(default)]
    pub vanished: Option<Vanished>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct Config {
    pub name: String,
    pub zfsroot: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub sshkey: Option<String>,
    /// Applied to every volume that does not set its own `vanished`.
    /// Absent means [`Vanished::Retry`].
    #[serde(default)]
    pub vanished: Vanished,
    // BTreeMap keeps a stable, deterministic processing order across runs.
    pub volumes: BTreeMap<String, Volume>,
}

fn default_port() -> u16 {
    22
}

/// SSH connection parameters shared across a config's volumes.
pub struct SshParams<'a> {
    pub host: &'a str,
    pub port: u16,
    pub sshkey: Option<&'a str>,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        info!("Loading config file [cfg]{}[/]", path.display());

        let content = fs::read_to_string(path).context(format!(
            "Unable to load config file [cfg]{}[/]",
            path.display()
        ))?;

        let cfg: Config = serde_yaml::from_str(&content).context(format!(
            "Unable to validate configuration file [cfg]{}[/]",
            path.display()
        ))?;

        if cfg.port < 1 {
            crate::error_msg!(
                "Invalid [cfg]port[/] {} in [cfg]{}[/]: must be between 1 and 65535",
                cfg.port,
                path.display()
            );
        }

        if let Some(ref key) = cfg.sshkey
            && !Path::new(key).exists()
        {
            warn!("SSH key [cfg]{}[/] does not seems to be present", key);
        }

        Ok(cfg)
    }

    pub fn ssh_params(&self) -> SshParams<'_> {
        SshParams {
            host: &self.host,
            port: self.port,
            sshkey: self.sshkey.as_deref(),
        }
    }
}
