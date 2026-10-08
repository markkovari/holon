//! Configuration and state. The config holds secrets (the appservice tokens, the
//! runtime's admin token, a Synapse admin token), so it lives OUTSIDE the repo,
//! mode 0600, and `init` is the only thing that writes it.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    /// Where the bridge reaches Synapse (`https://malna.tail3a9c.ts.net`).
    pub homeserver: String,
    /// Part of every user id; permanent for the server.
    pub server_name: String,
    /// The appservice tokens, shared with Synapse through the registration file.
    pub as_token: String,
    pub hs_token: String,
    /// Where the bridge listens for Synapse's transactions.
    pub listen: String,
    pub runtime_url: String,
    pub runtime_token: String,
    /// The one person who may talk to the agents (`@mark:server`).
    pub owner: String,
    /// The owner's own session token (from the one login `init` does). Used ONLY to
    /// accept the owner's invites into rooms the bridge creates, so they are already
    /// in your list: an appservice cannot act as a user outside its namespace. It is
    /// as sensitive as a password; the file is mode 0600.
    pub admin_token: String,
    pub state_file: PathBuf,
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let s = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        serde_json::from_str(&s).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let body = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        write_private(path, &body)
    }

    /// `@agent-<name>:<server>` — the Matrix user that IS this agent.
    pub fn ghost(&self, agent: &str) -> String {
        format!("@agent-{agent}:{}", self.server_name)
    }

    /// The inverse of `ghost`; `None` for anyone who is not an agent's user.
    pub fn agent_of(&self, user_id: &str) -> Option<String> {
        user_id
            .strip_prefix("@agent-")?
            .strip_suffix(&format!(":{}", self.server_name))
            .filter(|n| !n.is_empty())
            .map(String::from)
    }

    /// The bridge's own user (it creates spaces, speaks in the control room).
    pub fn bridge_user(&self) -> String {
        format!("@holon-bridge:{}", self.server_name)
    }
}

/// Writes a file readable by the owner only, atomically.
pub fn write_private(path: &Path, body: &str) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?;
    f.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            homeserver: "http://x".into(),
            server_name: "malna.ts.net".into(),
            as_token: "a".into(),
            hs_token: "h".into(),
            listen: "0.0.0.0:9009".into(),
            runtime_url: "http://r".into(),
            runtime_token: "t".into(),
            owner: "@mark:malna.ts.net".into(),
            admin_token: "x".into(),
            state_file: "/tmp/s.json".into(),
        }
    }

    #[test]
    fn ghost_ids_round_trip_and_only_agents_map_back() {
        let c = cfg();
        assert_eq!(c.ghost("weather-bot"), "@agent-weather-bot:malna.ts.net");
        assert_eq!(c.agent_of("@agent-weather-bot:malna.ts.net").as_deref(), Some("weather-bot"));
        assert_eq!(c.agent_of("@mark:malna.ts.net"), None);
        assert_eq!(c.agent_of("@agent-x:other.server"), None);
        assert_eq!(c.agent_of("@agent-:malna.ts.net"), None);
        assert_eq!(c.agent_of(&c.bridge_user()), None);
    }

    #[test]
    fn the_config_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("bridge.json");
        cfg().save(&p).unwrap();
        assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(Config::load(&p).unwrap().owner, "@mark:malna.ts.net");
    }
}
