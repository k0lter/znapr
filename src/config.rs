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
    /// Extra arguments inserted at the start of the `ssh` command passed to `rsync -e`.
    /// OpenSSH keeps the first value of each option, so these can override the built-in ones.
    /// Empty or whitespace is treated as absent.
    #[serde(default)]
    pub rsync_ssh_args: Option<String>,
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
    pub rsync_ssh_args: Option<&'a str>,
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
            rsync_ssh_args: self
                .rsync_ssh_args
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(args: Option<&str>) -> Config {
        Config {
            name: "n".into(),
            zfsroot: "tank".into(),
            host: "h".into(),
            port: 22,
            sshkey: None,
            rsync_ssh_args: args.map(str::to_string),
            vanished: Vanished::Retry,
            volumes: BTreeMap::new(),
        }
    }

    #[test]
    fn ssh_params_drops_blank_rsync_ssh_args() {
        assert_eq!(cfg(None).ssh_params().rsync_ssh_args, None);
        assert_eq!(cfg(Some("  ")).ssh_params().rsync_ssh_args, None);
        assert_eq!(cfg(Some("  -4  ")).ssh_params().rsync_ssh_args, Some("-4"));
    }
}
