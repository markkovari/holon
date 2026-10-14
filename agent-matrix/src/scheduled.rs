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
    // A schedule is the pair (cron, project): the same cron for two projects is two triggers.
    let notified = |cron: &str, project: &str| {
        triggers.iter().any(|t| {
            t["kind"] == "schedule"
                && t["cron"] == cron
                && t["notify"].as_bool().unwrap_or(false)
                && t["project"].as_str().unwrap_or("") == project
        })
    };
    // A run refused for want of project membership is a mistake the owner should hear of.
    let worth_saying =
        |r: &RunInfo| r.status != "dropped" || r.answer.contains("not a member of the project");
    let mut out: Vec<&RunInfo> = runs
        .iter()
        .filter(|r| r.finished_at_ms() > after_ms && worth_saying(r))
        .filter(|r| match r.trigger.strip_prefix("schedule: ") {
            Some(cron) => notified(cron, &r.project),
            None => r.trigger == "timer",
        })
        .collect();
    out.sort_by_key(|r| r.finished_at_ms());
    out
}

/// The room a project's agents report in: its feed, or its only room when it has no feed
/// (a project of one agent, or a Space the owner made).
fn report_room(project: &str, data: &Data) -> Option<String> {
    let r = data.projects.get(project)?;
    [&r.feed, &r.general].into_iter().find(|room| !room.is_empty()).cloned()
}

fn member_projects<'a>(agent: &str, projects: &'a [ProjectInfo]) -> Vec<&'a ProjectInfo> {
    let mut mine: Vec<&ProjectInfo> =
        projects.iter().filter(|p| p.agents.iter().any(|a| a == agent)).collect();
    mine.sort_by(|a, b| a.name.cmp(&b.name));
    mine
}

/// Where a result for no particular project goes: the agent's own room, and for an agent
/// in a project (which has none) the report room of its first project by name.
pub fn room_for(agent: &str, projects: &[ProjectInfo], data: &Data) -> Option<String> {
    data.dms.get(agent).cloned().or_else(|| {
        member_projects(agent, projects).iter().find_map(|p| report_room(&p.name, data))
    })
}

/// Where the result of `run` goes: the report room of the project it was for, else
/// `room_for`.
pub fn room_for_run(
    run: &RunInfo,
    agent: &str,
    projects: &[ProjectInfo],
    data: &Data,
) -> Option<String> {
    Some(run.project.as_str())
        .filter(|p| !p.is_empty())
        .and_then(|p| report_room(p, data))
        .or_else(|| room_for(agent, projects, data))
}

/// Where `agent`'s approval poll goes: its own room; an agent in a project has none, so
/// the project's general room, where you talk to it; else the control room.
pub fn approval_room(agent: &str, projects: &[ProjectInfo], data: &Data) -> Option<String> {
    data.dms
        .get(agent)
        .cloned()
        .or_else(|| {
            member_projects(agent, projects).iter().find_map(|p| {
                data.projects.get(&p.name).map(|r| r.general.clone()).filter(|g| !g.is_empty())
            })
        })
        .or_else(|| data.control_room.clone())
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
            project: String::new(),
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
        d.dms.remove("coach");
        assert_eq!(room_for("coach", &projects, &d).as_deref(), Some("!f:s"), "no room of its own");
        d.dms.insert("coach".into(), "!dm:s".into());
        assert_eq!(
            room_for("coach", &projects, &d).as_deref(),
            Some("!dm:s"),
            "its own room first"
        );
        assert_eq!(
            room_for("weather", &projects, &d).as_deref(),
            Some("!wdm:s"),
            "an adopted Space has no feed room, so the direct room it is"
        );
        assert_eq!(room_for("nobody", &projects, &d), None);
    }

    #[test]
    fn a_one_room_project_reports_in_its_only_room_and_approvals_follow_the_agent() {
        let mut d = Data { control_room: Some("!ctl:s".into()), ..Default::default() };
        d.projects.insert(
            "solo".into(),
            ProjectRooms { space: "!sp:s".into(), general: "!g:s".into(), feed: String::new() },
        );
        let projects = vec![ProjectInfo {
            name: "solo".into(),
            agents: vec!["coach".into()],
            ..Default::default()
        }];
        assert_eq!(room_for("coach", &projects, &d).as_deref(), Some("!g:s"));
        assert_eq!(
            approval_room("coach", &projects, &d).as_deref(),
            Some("!g:s"),
            "no room of its own"
        );
        d.dms.insert("coach".into(), "!own:s".into());
        assert_eq!(approval_room("coach", &projects, &d).as_deref(), Some("!own:s"));
        assert_eq!(approval_room("stray", &projects, &d).as_deref(), Some("!ctl:s"));
    }

    #[test]
    fn the_same_cron_for_two_projects_is_two_schedules_and_each_goes_to_its_project() {
        let triggers = vec![
            json!({"kind": "schedule", "cron": "@daily", "notify": true, "project": "rowing"}),
            json!({"kind": "schedule", "cron": "@daily", "notify": false, "project": "nutrition"}),
        ];
        let mut rowing = run("r", "schedule: @daily", "ok", 5_000);
        rowing.project = "rowing".into();
        let mut food = run("f", "schedule: @daily", "ok", 6_000);
        food.project = "nutrition".into();
        let own = run("o", "schedule: @daily", "ok", 7_000);
        let runs = vec![rowing.clone(), food, own];
        assert_eq!(
            ids(due(&triggers, &runs, 0)),
            ["r"],
            "only the one marked notify, in its project"
        );

        let mut d = Data::default();
        d.dms.insert("coach".into(), "!own:s".into());
        d.projects.insert(
            "rowing".into(),
            ProjectRooms { space: "!sp:s".into(), general: "!g:s".into(), feed: "!f:s".into() },
        );
        let projects = vec![ProjectInfo {
            name: "rowing".into(),
            agents: vec!["coach".into()],
            ..Default::default()
        }];
        assert_eq!(room_for_run(&rowing, "coach", &projects, &d).as_deref(), Some("!f:s"));
        assert_eq!(
            room_for_run(&runs[2], "coach", &projects, &d).as_deref(),
            Some("!own:s"),
            "no project: the agent's own room, not the first project's"
        );
    }

    #[test]
    fn a_refusal_for_want_of_membership_is_posted_and_other_drops_are_not() {
        let triggers = vec![
            json!({"kind": "schedule", "cron": "@daily", "notify": true, "project": "rowing"}),
        ];
        let mut refused = run("a", "schedule: @daily", "dropped", 2_000);
        refused.project = "rowing".into();
        refused.answer = "dropped: stranger is not a member of the project rowing".into();
        let mut limited = run("b", "schedule: @daily", "dropped", 3_000);
        limited.project = "rowing".into();
        limited.answer = "dropped: rate limit: 3 runs per minute".into();
        assert_eq!(ids(due(&triggers, &[refused, limited], 0)), ["a"]);
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
