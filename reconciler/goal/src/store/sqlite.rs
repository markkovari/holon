//! Solo / Local embedded SQLite goal storage implementation.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};

use super::{now_secs, CheckSpec, GoalRecord, GoalStatus, GoalStore, PartSpec};

/// Solo/Local embedded SQLite implementation: zero external processes or daemons needed.
pub struct SqliteGoalStore {
    conn: Mutex<Connection>,
    path: String,
}

impl SqliteGoalStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path_str = path.as_ref().to_string_lossy().to_string();
        if let Some(parent) = path.as_ref().parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating directory for {}", parent.display()))?;
            }
        }
        let conn = Connection::open(&path).with_context(|| {
            format!("opening SQLite goal database at {}", path.as_ref().display())
        })?;
        let store = Self { conn: Mutex::new(conn), path: path_str };
        store.init_schema()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().context("opening in-memory SQLite database")?;
        let store = Self { conn: Mutex::new(conn), path: ":memory:".into() };
        store.init_schema()?;
        Ok(store)
    }

    pub fn default_local_path() -> &'static str {
        ".holon/goals.db"
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    fn init_schema(&self) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute_batch(
            "
            PRAGMA journal_mode = WAL;
            PRAGMA busy_timeout = 5000;

            CREATE TABLE IF NOT EXISTS goals (
                id TEXT PRIMARY KEY,
                project TEXT NOT NULL,
                title TEXT NOT NULL,
                text TEXT NOT NULL,
                status TEXT NOT NULL,
                priority INTEGER NOT NULL DEFAULT 100,
                parent TEXT,
                writable_json TEXT NOT NULL DEFAULT '[]',
                context_json TEXT NOT NULL DEFAULT '[]',
                parts_json TEXT NOT NULL,
                checks_json TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_goals_dispatch
                ON goals (project, status, priority DESC, created_at ASC);
            ",
        )
        .context("initializing goals table schema")?;
        Ok(())
    }
}

impl GoalStore for SqliteGoalStore {
    fn is_shared(&self) -> bool {
        false
    }

    fn create_goal(&self, goal: &GoalRecord) -> Result<String> {
        let conn = self.conn.lock().unwrap();
        let writable_json = serde_json::to_string(&goal.writable)?;
        let context_json = serde_json::to_string(&goal.context)?;
        let parts_json = serde_json::to_string(&goal.parts)?;
        let checks_json = serde_json::to_string(&goal.composition_checks)?;

        conn.execute(
            "INSERT INTO goals (id, project, title, text, status, priority, parent, writable_json, context_json, parts_json, checks_json, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                goal.id,
                goal.project,
                goal.title,
                goal.text,
                goal.status.as_str(),
                goal.priority,
                goal.parent,
                writable_json,
                context_json,
                parts_json,
                checks_json,
                goal.created_at,
                goal.updated_at,
            ],
        )?;
        Ok(goal.id.clone())
    }

    fn get_goal(&self, id: &str) -> Result<Option<GoalRecord>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT id, project, title, text, status, priority, parent, writable_json, context_json, parts_json, checks_json, created_at, updated_at
             FROM goals WHERE id = ?1",
        )?;

        let res = stmt.query_row(params![id], map_row).optional()?;
        Ok(res)
    }

    fn list_goals(&self, project: &str, status: Option<GoalStatus>) -> Result<Vec<GoalRecord>> {
        let conn = self.conn.lock().unwrap();
        let (query, has_status) = if status.is_some() {
            (
                "SELECT id, project, title, text, status, priority, parent, writable_json, context_json, parts_json, checks_json, created_at, updated_at
                 FROM goals WHERE project = ?1 AND status = ?2
                 ORDER BY priority DESC, created_at ASC",
                true,
            )
        } else {
            (
                "SELECT id, project, title, text, status, priority, parent, writable_json, context_json, parts_json, checks_json, created_at, updated_at
                 FROM goals WHERE project = ?1
                 ORDER BY priority DESC, created_at ASC",
                false,
            )
        };

        let mut stmt = conn.prepare(query)?;
        let rows = if has_status {
            stmt.query_map(params![project, status.unwrap().as_str()], map_row)?
        } else {
            stmt.query_map(params![project], map_row)?
        };

        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    fn transition_status(&self, id: &str, from: GoalStatus, to: GoalStatus) -> Result<bool> {
        if !from.can_transition_to(to) {
            bail!("illegal goal transition from `{from}` to `{to}`");
        }
        let conn = self.conn.lock().unwrap();
        let now = now_secs();
        let changed = conn.execute(
            "UPDATE goals SET status = ?1, updated_at = ?2 WHERE id = ?3 AND status = ?4",
            params![to.as_str(), now, id, from.as_str()],
        )?;
        Ok(changed > 0)
    }

    fn claim_next_queued(&self, project: &str) -> Result<Option<GoalRecord>> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = now_secs();

        let goal_id: Option<String> = {
            let mut stmt = tx.prepare(
                "SELECT id FROM goals WHERE project = ?1 AND status = 'queued'
                 ORDER BY priority DESC, created_at ASC LIMIT 1",
            )?;
            stmt.query_row(params![project], |r| r.get(0)).optional()?
        };

        let Some(id) = goal_id else {
            return Ok(None);
        };

        tx.execute(
            "UPDATE goals SET status = 'running', updated_at = ?1 WHERE id = ?2",
            params![now, id],
        )?;

        let record = {
            let mut stmt = tx.prepare(
                "SELECT id, project, title, text, status, priority, parent, writable_json, context_json, parts_json, checks_json, created_at, updated_at
                 FROM goals WHERE id = ?1",
            )?;
            stmt.query_row(params![id], map_row)?
        };
        tx.commit()?;

        Ok(Some(record))
    }

    fn delete_goal(&self, id: &str) -> Result<bool> {
        let conn = self.conn.lock().unwrap();
        let changed = conn.execute("DELETE FROM goals WHERE id = ?1", params![id])?;
        Ok(changed > 0)
    }
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<GoalRecord> {
    let status_str: String = row.get(4)?;
    let status = GoalStatus::from_str_lossy(&status_str).unwrap_or(GoalStatus::Queued);
    let writable_raw: String = row.get(7)?;
    let context_raw: String = row.get(8)?;
    let parts_raw: String = row.get(9)?;
    let checks_raw: String = row.get(10)?;

    let writable: Vec<String> = serde_json::from_str(&writable_raw).unwrap_or_default();
    let context: Vec<String> = serde_json::from_str(&context_raw).unwrap_or_default();
    let parts: Vec<PartSpec> = serde_json::from_str(&parts_raw).unwrap_or_default();
    let checks: Vec<CheckSpec> = serde_json::from_str(&checks_raw).unwrap_or_default();

    Ok(GoalRecord {
        id: row.get(0)?,
        project: row.get(1)?,
        title: row.get(2)?,
        text: row.get(3)?,
        status,
        priority: row.get(5)?,
        parent: row.get(6)?,
        writable,
        context,
        parts,
        composition_checks: checks,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}
