//! Tunables from a file, so calibrating the platform's guessed constants needs no recompile.
//!
//! **Precedence: command line, then environment, then file, then default.** A flag typed while
//! debugging must beat a stale file, and a supervisor's environment variable must beat the file it
//! shipped with.
//!
//! TOML rather than YAML: the CLI already parses TOML specs and the only maintained Rust YAML crate
//! is a fork of an abandoned one.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Where a config file is looked for when `--config` is not given (ADR-0118).
pub const DEFAULT_PATH: &str = "holon.toml";
pub const FALLBACK_PATH: &str = "comp.toml";
pub const ENV_PATH: &str = "HOLON_CONFIG";
pub const FALLBACK_ENV_PATH: &str = "COMP_CONFIG";

#[derive(Debug, Default, Deserialize, PartialEq)]
// A silently ignored typo in a config file is worse than no file: unknown keys are an error.
#[serde(deny_unknown_fields)]
pub struct File {
    #[serde(default)]
    pub reconciler: Reconciler,
    #[serde(default)]
    pub ingress: Ingress,
}

/// Every field optional: absent means the flag default, so a file may set one value alone.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Reconciler {
    /// Seconds between reconcile passes.
    pub interval: Option<u64>,
    /// Consecutive passes a surplus must persist before anything is stopped. Deficits are acted on
    /// immediately.
    pub settle_passes: Option<u32>,
    /// Commands per pass, so a mass event drains instead of stampeding.
    pub max_commands: Option<usize>,
    /// Seconds to wait for a node to ack a command.
    pub command_timeout: Option<u64>,
    /// Seconds an inventory entry lives without a heartbeat.
    pub inventory_ttl: Option<u64>,
}

#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Ingress {
    /// Seconds between inventory refreshes of the route table. Absent means a third of
    /// `inventory_ttl`.
    pub refresh_secs: Option<u64>,
    /// How long an inventory entry lives. Must match the hosts and the reconciler: all three declare
    /// it on one shared bucket and the first to create it wins, silently.
    pub inventory_ttl: Option<u64>,
    /// Seconds to wait on a backend before giving up on it.
    pub backend_timeout: Option<u64>,
    /// Requests in flight to one node before shedding; 0 disables shedding.
    pub max_inflight: Option<usize>,
    /// How many SLOW backends one request may spend before giving up (refused connections are free).
    pub slow_budget: Option<usize>,
    /// Seconds to wait for a scaled-to-zero app to be activated.
    pub activation_timeout: Option<u64>,
}

impl File {
    /// Load from `path`, or from `$HOLON_CONFIG` / `$COMP_CONFIG`, or from `./holon.toml` / `./comp.toml`
    /// if it exists (ADR-0118). An explicitly named file that cannot be read is an error; the implicit
    /// one may simply not exist.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let (path, required) = match explicit {
            Some(p) => (p.to_path_buf(), true),
            None => {
                if let Ok(p) = std::env::var(ENV_PATH).or_else(|_| std::env::var(FALLBACK_ENV_PATH)) {
                    if !p.is_empty() {
                        (PathBuf::from(p), true)
                    } else if Path::new(DEFAULT_PATH).exists() {
                        (PathBuf::from(DEFAULT_PATH), false)
                    } else {
                        (PathBuf::from(FALLBACK_PATH), false)
                    }
                } else if Path::new(DEFAULT_PATH).exists() {
                    (PathBuf::from(DEFAULT_PATH), false)
                } else {
                    (PathBuf::from(FALLBACK_PATH), false)
                }
            }
        };
        if !path.exists() {
            if required {
                anyhow::bail!("no config file at {}", path.display());
            }
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: Self =
            toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
        eprintln!("config: {}", path.display());
        Ok(parsed)
    }
}

/// Pick a value: what the caller typed, else the file, else the default. `cli` is `None` when the
/// flag was left at its default. Pure so precedence is tested once.
pub fn pick<T>(cli: Option<T>, file: Option<T>, default: T) -> T {
    cli.or(file).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn precedence_is_cli_then_file_then_default() {
        assert_eq!(pick(Some(1), Some(2), 3), 1, "an explicit flag wins");
        assert_eq!(pick(None, Some(2), 3), 2, "then the file");
        assert_eq!(pick(None, None, 3), 3, "then the default");
    }

    #[test]
    fn a_partial_file_leaves_everything_else_alone() {
        // Tuning one field must leave every other `None` so the defaults still apply.
        let f: File = toml::from_str("[reconciler]\nsettle_passes = 5\n").unwrap();
        assert_eq!(f.reconciler.settle_passes, Some(5));
        assert_eq!(f.reconciler.interval, None);
        assert_eq!(f.ingress, Ingress::default());
    }

    #[test]
    fn a_misspelled_key_is_an_error_rather_than_silence() {
        // A config file whose typo is ignored is worse than no config file.
        let err = toml::from_str::<File>("[ingress]\nmax_inflght = 10\n")
            .expect_err("a typo must not parse");
        assert!(err.to_string().contains("max_inflght"), "{err}");

        let err = toml::from_str::<File>("[ingres]\nmax_inflight = 10\n")
            .expect_err("a misspelled section must not parse");
        assert!(err.to_string().contains("ingres"), "{err}");
    }

    #[test]
    fn an_empty_file_is_all_defaults_rather_than_an_error() {
        assert_eq!(toml::from_str::<File>("").unwrap(), File::default());
    }

    #[test]
    fn an_implicit_missing_file_is_fine_and_an_explicit_one_is_not() {
        let missing = Path::new("/nonexistent/comp.toml");
        assert!(File::load(Some(missing)).is_err(), "asked for a file that is not there");
        // The implicit lookup must not fail when there is simply no file.
        let dir = std::env::temp_dir().join("comp-settings-test");
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let loaded = File::load(None);
        std::env::set_current_dir(prev).unwrap();
        assert_eq!(loaded.unwrap(), File::default());
    }
}
