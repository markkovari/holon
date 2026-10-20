//! The config holds secrets: it lives outside the repo, mode 0600, written only by `init`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub homeserver: String,
    /// Permanent: it is part of every user id.
    pub server_name: String,
    pub as_token: String,
    pub hs_token: String,
    pub listen: String,
    pub runtime_url: String,
    pub runtime_token: String,
    /// The only person who may talk to the agents.
    pub owner: String,
    /// The owner's session token, used only to accept the owner's invites: an appservice
    /// cannot act as a user outside its namespace. As sensitive as a password.
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

    pub fn ghost(&self, agent: &str) -> String {
        format!("@agent-{agent}:{}", self.server_name)
    }

    pub fn agent_of(&self, user_id: &str) -> Option<String> {
        user_id
            .strip_prefix("@agent-")?
            .strip_suffix(&format!(":{}", self.server_name))
            .filter(|n| !n.is_empty())
            .map(String::from)
    }

    pub fn bridge_user(&self) -> String {
        format!("@holon-bridge:{}", self.server_name)
    }
}

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
