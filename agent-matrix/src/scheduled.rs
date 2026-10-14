//! Which finished runs the bridge posts to the owner without being asked.
//!
//! The runtime runs a schedule or a reminder on its own and only logs the result;
//! a chat message exists only when someone wrote one. So the bridge watches the
//! runs and posts the ones the owner opted into: a schedule trigger with
//! `"notify": true`, and every `schedule_self` reminder (asking to be reminded is
//! asking to be told). Everything here is pure; `Bridge::deliver_scheduled` does
//! the talking.

use serde_json::Value;

use crate::runtime::{ProjectInfo, RunInfo};
use crate::state::Data;

/// The runs to post, oldest first: finished after `after_ms`, woken by a reminder or
/// by a schedule `triggers` marks `notify`. A `dropped` run (refused by a loop guard
/// or a rate limit) is not a result and is left out.
pub fn due<'a>(triggers: &[Value], runs: &'a [RunInfo], after_ms: u64) -> Vec<&'a RunInfo> {
    let notified = |cron: &str| {
        triggers.iter().any(|t| {
            t["kind"] == "schedule" && t["cron"] == cron && t["notify"].as_bool().unwrap_or(false)
        })
    };
    let mut out: Vec<&RunInfo> = runs
        .iter()
        .filter(|r| r.finished_at_ms() > after_ms && r.status != "dropped")
        .filter(|r| match r.trigger.strip_prefix("schedule: ") {
            Some(cron) => notified(cron),
            None => r.trigger == "timer",
        })
        .collect();
    out.sort_by_key(|r| r.finished_at_ms());
    out
}

/// Where `agent`'s results go: the feed room of the first project it belongs to that
/// has one, else its direct room with the owner.
pub fn room_for(agent: &str, projects: &[ProjectInfo], data: &Data) -> Option<String> {
    let mut mine: Vec<&ProjectInfo> =
        projects.iter().filter(|p| p.agents.iter().any(|a| a == agent)).collect();
    mine.sort_by(|a, b| a.name.cmp(&b.name));
    mine.iter()
        .find_map(|p| data.projects.get(&p.name).map(|r| r.feed.clone()).filter(|f| !f.is_empty()))
        .or_else(|| data.dms.get(agent).cloned())
}

/// The message: the answer as it is, or why there is none.
pub fn render(run: &RunInfo, max: usize) -> String {
    let answer = run.answer.trim();
    match run.status.as_str() {
        "ok" if answer.is_empty() => "(no answer)".to_string(),
        "ok" => crate::bridge::clip(answer, max),
        "over_budget" => format!("⚠️ stopped over budget: {}", crate::bridge::clip(answer, 2_000)),
        _ => format!("⚠️ {}", crate::bridge::clip(answer, 2_000)),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::state::ProjectRooms;

    fn run(id: &str, trigger: &str, status: &str, ms: u64) -> RunInfo {
        RunInfo {
            id: id.into(),
            trigger: trigger.into(),
            status: status.into(),
            answer: format!("answer {id}"),
            finished: ms / 1000,
            finished_ms: ms,
        }
    }

    fn ids(v: Vec<&RunInfo>) -> Vec<&str> {
        v.into_iter().map(|r| r.id.as_str()).collect()
    }

    #[test]
    fn only_opted_in_schedules_and_reminders_are_posted_oldest_first() {
        let triggers = vec![
            json!({"kind": "schedule", "cron": "0 12 * * *", "prompt": "p", "notify": true}),
            json!({"kind": "schedule", "cron": "@every 1h", "prompt": "p"}),
            json!({"kind": "event", "topic": "t"}),
        ];
        let runs = vec![
            run("hourly", "schedule: @every 1h", "ok", 5_000),
            run("remind", "timer", "ok", 4_000),
            run("daily", "schedule: 0 12 * * *", "ok", 3_000),
            run("chat", "http", "ok", 6_000),
            run("event", "event: t", "ok", 7_000),
            run("old", "schedule: 0 12 * * *", "ok", 1_000),
        ];
        assert_eq!(ids(due(&triggers, &runs, 2_000)), ["daily", "remind"]);
        assert!(due(&triggers, &runs, 9_000).is_empty(), "nothing newer than the watermark");
    }

    #[test]
    fn a_failed_run_is_posted_and_a_dropped_one_is_not() {
        let triggers = vec![json!({"kind": "schedule", "cron": "@daily", "notify": true})];
        let runs = vec![
            run("bad", "schedule: @daily", "failed", 2_000),
            run("loop", "schedule: @daily", "dropped", 3_000),
        ];
        assert_eq!(ids(due(&triggers, &runs, 0)), ["bad"]);
        assert!(render(&runs[0], 100).starts_with("⚠️ "));
    }

    #[test]
    fn a_result_goes_to_the_projects_feed_else_the_direct_room() {
        let mut d = Data::default();
        d.dms.insert("coach".into(), "!dm:s".into());
        d.dms.insert("weather".into(), "!wdm:s".into());
        d.projects.insert(
            "rowing".into(),
            ProjectRooms { space: "!sp:s".into(), general: "!g:s".into(), feed: "!f:s".into() },
        );
        d.projects.insert(
            "adopted".into(),
            ProjectRooms { space: "!a:s".into(), general: String::new(), feed: String::new() },
        );
        let projects = vec![
            ProjectInfo {
                name: "rowing".into(),
                agents: vec!["coach".into()],
                ..Default::default()
            },
            ProjectInfo {
                name: "adopted".into(),
                agents: vec!["weather".into()],
                ..Default::default()
            },
        ];
        assert_eq!(room_for("coach", &projects, &d).as_deref(), Some("!f:s"));
        assert_eq!(
            room_for("weather", &projects, &d).as_deref(),
            Some("!wdm:s"),
            "an adopted Space has no feed room, so the direct room it is"
        );
        assert_eq!(room_for("nobody", &projects, &d), None);
    }

    #[test]
    fn an_empty_answer_says_so() {
        let mut r = run("x", "timer", "ok", 1);
        r.answer = "  ".into();
        assert_eq!(render(&r, 100), "(no answer)");
    }

    /// The shapes the live runtime really sends, so a rename there fails here.
    #[test]
    fn the_runtimes_own_json_parses_and_selects() {
        let run: RunInfo = serde_json::from_str(r#"{"id": "coach-1791979200-372", "agent": "coach", "trigger": "schedule: 0 12 * * *", "input": "x", "started": 1791979200, "finished": 1791979201, "status": "ok", "answer": "Morning, Mark.", "steps": [], "tokens_in": 798, "tokens_out": 33, "started_ms": 1791979200029, "finished_ms": 1791979201313, "model": "openai/deepseek-chat", "trace_id": "91d5adc0e499a22eab1bc7d44ef0a245", "span_id": "26d1119f8e4ca9e9", "parent_span_id": null, "hops": 0, "chain": []}"#).unwrap();
        let agent: crate::runtime::AgentInfo = serde_json::from_str(r#"{"name": "coach", "description": "d", "triggers": [{"kind": "schedule", "cron": "0 12 * * *", "prompt": "Wake Mark with one short motivational message, in character as his rowing coach. Keep it to one or two sentences, warm and honest, no invented numbers.", "notify": true}]}"#).unwrap();
        assert_eq!(ids(due(&agent.triggers, &[run], 0)), ["coach-1791979200-372"]);
    }
}
