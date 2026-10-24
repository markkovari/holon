//! Durable goal storage and queue abstraction (ADR-0082, ADR-0091).
//! Replaces file-based .comp/goals/*.md and TOML parsing with structured SQL / NATS storage.

pub mod distributed;
pub mod sqlite;

pub use distributed::SurrealNatsGoalStore;
pub use sqlite::SqliteGoalStore;

use std::fmt;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum GoalStatus {
    Queued,
    Running,
    AwaitingHuman,
    Done,
    Failed,
    Abandoned,
}

impl GoalStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::AwaitingHuman => "awaiting-human",
            Self::Done => "done",
            Self::Failed => "failed",
            Self::Abandoned => "abandoned",
        }
    }

    pub fn from_str_lossy(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "awaiting-human" => Some(Self::AwaitingHuman),
            "done" => Some(Self::Done),
            "failed" => Some(Self::Failed),
            "abandoned" => Some(Self::Abandoned),
            _ => None,
        }
    }

    /// State transitions match platform-domain's legal moves (ADR-0082).
    pub fn can_transition_to(&self, target: GoalStatus) -> bool {
        matches!(
            (self, target),
            (Self::Queued, Self::Running)
                | (Self::Queued, Self::Abandoned)
                | (Self::Running, Self::AwaitingHuman)
                | (Self::Running, Self::Failed)
                | (Self::Running, Self::Abandoned)
                | (Self::AwaitingHuman, Self::Done)
                | (Self::AwaitingHuman, Self::Failed)
                | (Self::AwaitingHuman, Self::Abandoned)
        )
    }
}

impl fmt::Display for GoalStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckSpec {
    pub id: String,
    pub command: Vec<String>,
    pub required: bool,
    pub weight: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PartSpec {
    pub name: String,
    pub text: String,
    pub writable: Vec<String>,
    pub context: Vec<String>,
    #[serde(default)]
    pub checks: Vec<CheckSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GoalRecord {
    pub id: String,
    pub project: String,
    pub title: String,
    pub text: String,
    pub status: GoalStatus,
    pub priority: i32,
    pub parent: Option<String>,
    #[serde(default)]
    pub writable: Vec<String>,
    #[serde(default)]
    pub context: Vec<String>,
    #[serde(default)]
    pub parts: Vec<PartSpec>,
    #[serde(default)]
    pub composition_checks: Vec<CheckSpec>,
    pub created_at: u64,
    pub updated_at: u64,
}

impl GoalRecord {
    pub fn new(
        project: impl Into<String>,
        title: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        let now = now_secs();
        let title_str = title.into();
        let id = format!("{}-{}", slugify(&title_str), uuid_short());
        Self {
            id,
            project: project.into(),
            title: title_str,
            text: text.into(),
            status: GoalStatus::Queued,
            priority: 100,
            parent: None,
            writable: Vec::new(),
            context: Vec::new(),
            parts: Vec::new(),
            composition_checks: Vec::new(),
            created_at: now,
            updated_at: now,
        }
    }
}

/// Unified storage and queue trait for both Solo (SQLite) and Distributed (SurrealDB/NATS) modes.
pub trait GoalStore: Send + Sync {
    /// True when the store is shared across the distributed lattice.
    fn is_shared(&self) -> bool;

    /// Put a new goal into the store.
    fn create_goal(&self, goal: &GoalRecord) -> Result<String>;

    /// Read a goal by ID.
    fn get_goal(&self, id: &str) -> Result<Option<GoalRecord>>;

    /// List goals for a project with optional status filter.
    fn list_goals(&self, project: &str, status: Option<GoalStatus>) -> Result<Vec<GoalRecord>>;

    /// Transition a goal's status atomically. Returns true if transition was executed.
    fn transition_status(&self, id: &str, from: GoalStatus, to: GoalStatus) -> Result<bool>;

    /// Atomically claim the highest-priority queued goal for execution.
    fn claim_next_queued(&self, project: &str) -> Result<Option<GoalRecord>>;

    /// Delete or purge a goal.
    fn delete_goal(&self, id: &str) -> Result<bool>;
}

/// Auto-selects the storage backend based on explicit mode or detected environment.
pub fn build_goal_store(
    mode: Option<&str>,
    sqlite_path: Option<&str>,
    surreal_url: Option<&str>,
    nats_url: Option<&str>,
) -> Result<Arc<dyn GoalStore>> {
    let mode_str = mode.unwrap_or_else(|| {
        if std::env::var("HOLON_LATTICE").is_ok()
            || std::env::var("SURREAL_URL").is_ok()
            || std::env::var("NATS_URL").is_ok()
        {
            "distributed"
        } else {
            "solo"
        }
    });

    match mode_str {
        "distributed" | "lattice" => {
            let s_url = surreal_url
                .map(|s| s.to_string())
                .or_else(|| std::env::var("SURREAL_URL").ok())
                .unwrap_or_else(|| SurrealNatsGoalStore::default_endpoint().to_string());

            let n_url = nats_url.map(|s| s.to_string()).or_else(|| std::env::var("NATS_URL").ok());

            let ns = std::env::var("SURREAL_NS").unwrap_or_else(|_| "holon".into());
            let db = std::env::var("SURREAL_DB").unwrap_or_else(|_| "holon".into());
            let user = std::env::var("SURREAL_USER").unwrap_or_else(|_| "root".into());
            let password = std::env::var("SURREAL_PASS").ok().or(Some("root".into()));

            Ok(Arc::new(SurrealNatsGoalStore::new(s_url, ns, db, user, password, n_url)))
        }
        _ => {
            let path = sqlite_path.unwrap_or(SqliteGoalStore::default_local_path());
            Ok(Arc::new(SqliteGoalStore::open(path)?))
        }
    }
}

pub(crate) fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}

pub(crate) fn slugify(text: &str) -> String {
    let mut out = String::new();
    for ch in text.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let res = out.trim_matches('-');
    if res.is_empty() {
        "goal".into()
    } else {
        res.chars().take(40).collect()
    }
}

pub(crate) fn uuid_short() -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_le_bytes());
    format!("{:x}", h.finalize())[..8].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_status_transitions() {
        assert!(GoalStatus::Queued.can_transition_to(GoalStatus::Running));
        assert!(GoalStatus::Queued.can_transition_to(GoalStatus::Abandoned));
        assert!(!GoalStatus::Queued.can_transition_to(GoalStatus::Done));

        assert!(GoalStatus::Running.can_transition_to(GoalStatus::AwaitingHuman));
        assert!(GoalStatus::Running.can_transition_to(GoalStatus::Failed));
        assert!(!GoalStatus::Running.can_transition_to(GoalStatus::Queued));
    }

    #[test]
    fn sqlite_store_create_read_transition() -> Result<()> {
        let store = SqliteGoalStore::in_memory()?;
        let mut goal = GoalRecord::new("holon", "Triage API", "Finish defect triage API");
        goal.parts.push(PartSpec {
            name: "workflow".into(),
            text: "Implement workflow.rs".into(),
            writable: vec!["src/workflow.rs".into()],
            context: vec!["src/lib.rs".into()],
            checks: vec![CheckSpec {
                id: "workflow-pass".into(),
                command: vec!["cargo".into(), "test".into()],
                required: true,
                weight: 1,
            }],
        });

        let id = store.create_goal(&goal)?;
        assert_eq!(id, goal.id);

        let loaded = store.get_goal(&id)?.expect("goal exists");
        assert_eq!(loaded.title, "Triage API");
        assert_eq!(loaded.status, GoalStatus::Queued);
        assert_eq!(loaded.parts.len(), 1);
        assert_eq!(loaded.parts[0].name, "workflow");

        let ok = store.transition_status(&id, GoalStatus::Queued, GoalStatus::Running)?;
        assert!(ok);

        let loaded_running = store.get_goal(&id)?.unwrap();
        assert_eq!(loaded_running.status, GoalStatus::Running);

        let err = store.transition_status(&id, GoalStatus::Running, GoalStatus::Queued);
        assert!(err.is_err());

        Ok(())
    }

    #[test]
    fn atomic_claim_claims_highest_priority() -> Result<()> {
        let store = SqliteGoalStore::in_memory()?;
        let mut g1 = GoalRecord::new("proj-a", "Low Priority", "low");
        g1.priority = 10;
        let mut g2 = GoalRecord::new("proj-a", "High Priority", "high");
        g2.priority = 200;

        store.create_goal(&g1)?;
        store.create_goal(&g2)?;

        let claimed1 = store.claim_next_queued("proj-a")?.expect("claimed 1");
        assert_eq!(claimed1.title, "High Priority");
        assert_eq!(claimed1.status, GoalStatus::Running);

        let claimed2 = store.claim_next_queued("proj-a")?.expect("claimed 2");
        assert_eq!(claimed2.title, "Low Priority");

        let empty = store.claim_next_queued("proj-a")?;
        assert!(empty.is_none());

        Ok(())
    }

    #[test]
    fn build_goal_store_picks_solo_by_default() -> Result<()> {
        let store = build_goal_store(Some("solo"), Some(":memory:"), None, None)?;
        assert!(!store.is_shared(), "solo store is local/not shared");
        Ok(())
    }

    #[test]
    fn build_goal_store_picks_distributed_when_requested() -> Result<()> {
        let store = build_goal_store(
            Some("distributed"),
            None,
            Some("http://127.0.0.1:8000"),
            Some("nats://127.0.0.1:4222"),
        )?;
        assert!(store.is_shared(), "distributed store declares shared");
        Ok(())
    }
}
