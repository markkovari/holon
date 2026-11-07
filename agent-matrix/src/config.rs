//! Four of these are secrets: the component reads them from its secrets, or its config file.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Config {
    pub homeserver: String,
    /// Permanent: it is part of every user id.
    pub server_name: String,
    pub as_token: String,
    pub hs_token: String,
    pub runtime_url: String,
    pub runtime_token: String,
    /// The only person who may talk to the agents.
    pub owner: String,
    /// The owner's session token, used only to accept the owner's invites: an appservice
    /// cannot act as a user outside its namespace. As sensitive as a password.
    pub admin_token: String,
    /// How much of the conversation an agent is handed with a message; 0 sends it bare.
    #[serde(default = "history_chars")]
    pub history_chars: usize,
    /// Routes a project message to the members it concerns (TypeSafe's Jev); without it, embeddings.
    #[serde(default)]
    pub jev_key: Option<String>,
}

pub fn history_chars() -> usize {
    3000
}

impl Config {
    /// Every setting under its config key; `jev-key` only when there is one.
    pub fn entries(&self) -> Vec<(&'static str, String)> {
        let mut e = vec![
            ("matrix-homeserver", self.homeserver.clone()),
            ("matrix-server-name", self.server_name.clone()),
            ("matrix-owner", self.owner.clone()),
            ("matrix-as-token", self.as_token.clone()),
            ("matrix-hs-token", self.hs_token.clone()),
            ("matrix-admin-token", self.admin_token.clone()),
            ("runtime-url", self.runtime_url.clone()),
            ("runtime-token", self.runtime_token.clone()),
            ("history-chars", self.history_chars.to_string()),
        ];
        if let Some(k) = &self.jev_key {
            e.push(("jev-key", k.clone()));
        }
        e
    }

    /// The inverse of [`Config::entries`], from whatever answers a key (secrets, then config).
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let need = |k: &str| get(k).filter(|v| !v.is_empty()).ok_or(format!("{k} is not set"));
        Ok(Self {
            homeserver: need("matrix-homeserver")?,
            server_name: need("matrix-server-name")?,
            owner: need("matrix-owner")?,
            as_token: need("matrix-as-token")?,
            hs_token: need("matrix-hs-token")?,
            admin_token: need("matrix-admin-token")?,
            runtime_url: need("runtime-url")?,
            runtime_token: need("runtime-token")?,
            history_chars: get("history-chars")
                .and_then(|v| v.parse().ok())
                .unwrap_or_else(history_chars),
            jev_key: get("jev-key").filter(|v| !v.is_empty()),
        })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> Config {
        Config {
            homeserver: "http://x".into(),
            server_name: "malna.ts.net".into(),
            as_token: "a".into(),
            hs_token: "h".into(),
            runtime_url: "http://r".into(),
            runtime_token: "t".into(),
            owner: "@mark:malna.ts.net".into(),
            admin_token: "x".into(),
            history_chars: 3000,
            jev_key: None,
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
}
