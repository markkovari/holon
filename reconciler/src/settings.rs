//! Tunables from a file, so the numbers that were guesses stop being recompiles.
//!
//! Every constant in this platform arrived as a guess with a comment admitting it —
//! two settle passes, twenty commands a pass, sixty-four in flight, a two-timeout
//! retry budget. They were flags where someone remembered and `const` where nobody
//! did. A file makes the whole set visible in one place and calibratable without a
//! rebuild.
//!
//! **Precedence: command line, then environment, then file, then default.** The
//! surprising order is the useful one — a flag typed by a human debugging at 3am must
//! beat a file written months ago, and an environment variable set by whatever
//! supervises the process must beat the file it shipped with.
//!
//! TOML rather than YAML, deliberately: the CLI already parses TOML specs, and the
//! only maintained YAML crate for Rust is a fork of an abandoned one. Same shape
//! either way — say so and it is a small change.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// Where a config file is looked for when `--config` is not given.
pub const DEFAULT_PATH: &str = "comp.toml";
pub const ENV_PATH: &str = "COMP_CONFIG";

#[derive(Debug, Default, Deserialize, PartialEq)]
// A typo in a config file that is silently ignored is worse than no config file at
// all: the operator believes they changed something. Unknown keys are an error.
#[serde(deny_unknown_fields)]
pub struct File {
    #[serde(default)]
    pub reconciler: Reconciler,
    #[serde(default)]
    pub ingress: Ingress,
}

/// Every field optional: absent means "whatever the flag default is", so a file may
/// set one value without restating the rest.
#[derive(Debug, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Reconciler {
    /// Seconds between reconcile passes.
    pub interval: Option<u64>,
    /// Consecutive passes a surplus must persist before anything is stopped — the
    /// cooldown. Deficits are acted on immediately; only shrinking waits.
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
    /// Seconds between inventory refreshes of the route table. Absent means a
    /// third of `inventory_ttl`, which is the only value that is safe by
    /// construction.
    pub refresh_secs: Option<u64>,
    /// How long an inventory entry lives. Must match the hosts and the
    /// reconciler: all three declare it on one shared bucket and the first to
    /// create it wins, silently.
    pub inventory_ttl: Option<u64>,
    /// Seconds to wait on a backend before giving up on it.
    pub backend_timeout: Option<u64>,
    /// Requests in flight to one node before shedding; 0 disables shedding.
    pub max_inflight: Option<usize>,
    /// How many SLOW backends one request may spend before giving up. Refused
    /// connections are skipped for free and are not counted against this.
    pub slow_budget: Option<usize>,
    /// Seconds to wait for a scaled-to-zero app to be activated.
    pub activation_timeout: Option<u64>,
}

impl File {
    /// Load from `path`, or from `$COMP_CONFIG`, or from `./comp.toml` if it exists.
    ///
    /// An explicitly named file that cannot be read is an error — someone asked for
    /// it. The implicit one simply may not exist, which is the normal case and not
    /// worth a word.
    pub fn load(explicit: Option<&Path>) -> Result<Self> {
        let (path, required) = match explicit {
            Some(p) => (p.to_path_buf(), true),
            None => match std::env::var(ENV_PATH) {
                Ok(p) if !p.is_empty() => (PathBuf::from(p), true),
                _ => (PathBuf::from(DEFAULT_PATH), false),
            },
        };
        if !path.exists() {
            if required {
                anyhow::bail!("no config file at {}", path.display());
            }
            return Ok(Self::default());
        }
        let text = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let parsed: Self = toml::from_str(&text)
            .with_context(|| format!("parsing {}", path.display()))?;
        eprintln!("config: {}", path.display());
        Ok(parsed)
    }
}

/// Pick a value: what the caller typed, else the file, else the default.
///
/// `cli` is `None` when the flag was left at its default, which is how clap's own
/// defaults are kept out of the way of the file. A pure function so precedence is
/// tested once rather than reasoned about at every call site.
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
        // The common case: someone tunes the cooldown and nothing else. Every other
        // field must stay None so the defaults still apply.
        let f: File = toml::from_str("[reconciler]\nsettle_passes = 5\n").unwrap();
        assert_eq!(f.reconciler.settle_passes, Some(5));
        assert_eq!(f.reconciler.interval, None);
        assert_eq!(f.ingress, Ingress::default());
    }

    #[test]
    fn a_misspelled_key_is_an_error_rather_than_silence() {
        // THE property worth testing here. A config file whose typo is ignored is
        // worse than no config file: the operator believes they changed something.
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
        // The implicit lookup must not fail when there is simply no file, which is
        // how every existing deployment runs today.
        let dir = std::env::temp_dir().join("comp-settings-test");
        std::fs::create_dir_all(&dir).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let loaded = File::load(None);
        std::env::set_current_dir(prev).unwrap();
        assert_eq!(loaded.unwrap(), File::default());
    }
}
