//! Control-room `!` commands, so clients that cannot create Spaces can still manage projects.

use crate::bridge::{clip, Bridge};
use crate::runtime::ProjectInfo;

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Help,
    Agents,
    AgentTrash,
    AgentRestore { name: String },
    AgentPurge { name: String },
    AgentRetire { name: String },
    ProjectList,
    ProjectNew { name: String, description: String },
    ProjectAdd { name: String, agents: Vec<String> },
    ProjectRm { name: String, agents: Vec<String> },
    ProjectLead { name: String, agent: Option<String> },
    ProjectDelete { name: String },
    ProjectArchive { name: String },
    ProjectUnarchive { name: String },
    Model { agent: Option<String>, model: Option<String> },
    Diff { agent: Option<String> },
    Steer { agent: Option<String>, message: String },
    Bad(String),
}

fn agent_token(t: &str) -> String {
    let t = t.trim_start_matches('@');
    let t = t.split(':').next().unwrap_or(t);
    t.strip_prefix("agent-").unwrap_or(t).to_string()
}

pub fn parse(body: &str) -> Option<Cmd> {
    let body = body.trim();
    let prefix = if body.starts_with('!') {
        '!'
    } else if body.starts_with('/') {
        '/'
    } else {
        return None;
    };
    let mut words = body[1..].split_whitespace();
    let word = |w: Option<&str>| w.unwrap_or_default().to_string();
    Some(match (words.next(), words.next()) {
        (Some("help"), _) | (Some("?"), _) => Cmd::Help,
        (Some("model"), first) => {
            let first_raw = first.unwrap_or_default();
            let second = words.next().map(str::to_string);
            if first_raw.starts_with('@') {
                let agent = agent_token(first_raw);
                Cmd::Model { agent: Some(agent), model: second }
            } else {
                match (first_raw.is_empty(), second) {
                    (true, _) => Cmd::Model { agent: None, model: None },
                    (false, Some(m)) => {
                        Cmd::Model { agent: Some(agent_token(first_raw)), model: Some(m) }
                    }
                    (false, None) => Cmd::Model { agent: None, model: Some(first_raw.to_string()) },
                }
            }
        }
        (Some("diff"), first) => {
            let agent = first.filter(|s| !s.is_empty()).map(agent_token);
            Cmd::Diff { agent }
        }
        (Some("steer"), first) => {
            let first_raw = first.unwrap_or_default();
            if first_raw.starts_with('@') {
                let agent = agent_token(first_raw);
                let message = words.collect::<Vec<_>>().join(" ");
                if message.is_empty() {
                    Cmd::Bad(format!("usage: {prefix}steer @agent <guidance message>"))
                } else {
                    Cmd::Steer { agent: Some(agent), message }
                }
            } else {
                let rest = words.collect::<Vec<_>>().join(" ");
                let message = if rest.is_empty() {
                    first_raw.to_string()
                } else {
                    format!("{first_raw} {rest}")
                };
                if message.is_empty() {
                    Cmd::Bad(format!("usage: {prefix}steer [agent] <guidance message>"))
                } else {
                    Cmd::Steer { agent: None, message }
                }
            }
        }
        (Some("agents"), _) => Cmd::Agents,
        (Some("agent"), Some(sub)) => match sub {
            "trash" => Cmd::AgentTrash,
            "restore" | "purge" | "retire" => {
                let name = agent_token(&word(words.next()));
                if name.is_empty() {
                    Cmd::Bad(format!("usage: !agent {sub} <agent>"))
                } else if sub == "restore" {
                    Cmd::AgentRestore { name }
                } else if sub == "retire" {
                    Cmd::AgentRetire { name }
                } else {
                    Cmd::AgentPurge { name }
                }
            }
            other => Cmd::Bad(format!("unknown agent command `{other}`; try !help")),
        },
        (Some("project"), Some(sub)) | (Some("projects"), Some(sub)) => match sub {
            "list" | "ls" => Cmd::ProjectList,
            "new" | "create" => {
                let name = word(words.next());
                let description = words.collect::<Vec<_>>().join(" ");
                if name.is_empty() {
                    Cmd::Bad("usage: !project new <name> [description]".into())
                } else {
                    Cmd::ProjectNew { name, description }
                }
            }
            "add" | "rm" | "remove" => {
                let name = word(words.next());
                let agents: Vec<String> =
                    words.map(agent_token).filter(|a| !a.is_empty()).collect();
                if name.is_empty() || agents.is_empty() {
                    Cmd::Bad(format!("usage: !project {sub} <project> <agent> [<agent>…]"))
                } else if sub == "add" {
                    Cmd::ProjectAdd { name, agents }
                } else {
                    Cmd::ProjectRm { name, agents }
                }
            }
            "lead" => {
                let name = word(words.next());
                let who = word(words.next());
                if name.is_empty() || who.is_empty() {
                    Cmd::Bad("usage: !project lead <project> <agent|none>".into())
                } else {
                    Cmd::ProjectLead { name, agent: (who != "none").then(|| agent_token(&who)) }
                }
            }
            "delete" => {
                let name = word(words.next());
                if name.is_empty() {
                    Cmd::Bad("usage: !project delete <project>".into())
                } else {
                    Cmd::ProjectDelete { name }
                }
            }
            "archive" | "unarchive" => {
                let name = word(words.next());
                if name.is_empty() {
                    Cmd::Bad(format!("usage: !project {sub} <project>"))
                } else if sub == "archive" {
                    Cmd::ProjectArchive { name }
                } else {
                    Cmd::ProjectUnarchive { name }
                }
            }
            other => Cmd::Bad(format!("unknown project command `{other}`; try {prefix}help")),
        },
        (Some(other), _) => {
            Cmd::Bad(format!("unknown command `{prefix}{other}`; try {prefix}help"))
        }
        (None, _) => Cmd::Help,
    })
}

pub const HELP: &str = "\
!agents                                  list the agents
!model [agent] [name]                    view or switch an agent's model
!diff [agent]                            show workspace diff
!steer [agent] <guidance>                send steering guidance to an agent
!agent trash                             deleted agents you can still restore (30 days)
!agent restore <agent>                   bring a deleted agent back, as it was
!agent purge <agent>                     remove a deleted agent from the trash for good
!agent retire <agent>                    end the room of an agent the runtime no longer lists
!project list                            list projects and who is in them
!project new <name> [description]        make a project (a Space with a general and a feed room)
!project add <name> <agent> [<agent>…]   add agents to a project
!project rm <name> <agent> [<agent>…]    take agents out
!project lead <name> <agent|none>        who answers when nobody is mentioned
!project delete <name>                   forget the project (its rooms stay)
!project archive <name>                  pause its agents and take its rooms out of your list
!project unarchive <name>                bring it back: agents resumed, rooms invited back

Commands can also be run with a slash (/model, /diff, /steer, /help).";

/// The runtime empties the trash after 30 days.
fn restorable_days(deleted_at: u64) -> u64 {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(deleted_at);
    (deleted_at + 30 * 86_400).saturating_sub(now) / 86_400
}

impl Bridge {
    pub fn command(&self, body: &str) -> Option<String> {
        let Some(cmd) = parse(body) else {
            return Some("I only understand commands here. Try !help or /help".into());
        };
        let reply = match cmd {
            Cmd::Help => HELP.to_string(),
            Cmd::Bad(e) => e,
            Cmd::Model { agent, model } => match agent {
                Some(a) => self.handle_model_cmd(&a, model.as_deref()),
                None => match model {
                    Some(m) => format!("Please specify which agent to update: /model <agent> {m}"),
                    None => "usage: /model <agent> [new_model]".into(),
                },
            },
            Cmd::Diff { agent } => match agent {
                Some(a) => self.handle_diff_cmd(&a),
                None => "usage: /diff <agent>".into(),
            },
            Cmd::Steer { agent, message } => match agent {
                Some(a) => self.handle_steer_cmd(&a, &message, None),
                None => "usage: /steer <agent> <guidance message>".into(),
            },
            Cmd::Agents => match self.rt.agents() {
                Ok(a) if a.is_empty() => "no agents yet".into(),
                Ok(a) => a.iter().map(|x| format!("{} — {}", x.name, x.description)).collect::<Vec<_>>().join("\n"),
                Err(e) => format!("could not list agents: {e}"),
            },
            Cmd::AgentTrash => match self.rt.trash() {
                Ok(t) if t.is_empty() => "the trash is empty".into(),
                Ok(t) => t
                    .iter()
                    .map(|a| {
                        format!(
                            "{} — {} recorded runs{}; restorable for {} more days",
                            a.name,
                            a.runs,
                            if a.projects.is_empty() {
                                String::new()
                            } else {
                                format!(", was in {}", a.projects.join(", "))
                            },
                            restorable_days(a.deleted_at)
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => format!("could not read the trash: {e}"),
            },
            Cmd::AgentRestore { name } => match self.rt.restore(&name) {
                Ok(t) => {
                    let _ = self.reconcile();
                    format!("{} is back with its {} recorded runs, memory and schedules.", t.name, t.runs)
                }
                Err(e) => format!("could not restore {name}: {e}"),
            },
            Cmd::AgentRetire { name } => self.retire_now(&name),
            Cmd::AgentPurge { name } => match self.rt.purge(&name) {
                Ok(t) => format!("{} is gone for good ({} recorded runs).", t.name, t.runs),
                Err(e) => format!("could not purge {name}: {e}"),
            },
            Cmd::ProjectList => match self.rt.projects() {
                Ok(p) if p.is_empty() => "no projects yet. !project new <name>".into(),
                Ok(p) => p
                    .iter()
                    .map(|p| {
                        format!(
                            "{}{} — {}{}",
                            p.name,
                            if p.archived { " [archived]" } else { "" },
                            if p.agents.is_empty() { "(no agents)".to_string() } else { p.agents.join(", ") },
                            p.lead.as_ref().map(|l| format!("  [lead: {l}]")).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => format!("could not list projects: {e}"),
            },
            Cmd::ProjectNew { name, description } => {
                match self.rt.put_project(&ProjectInfo { name: name.clone(), description, ..Default::default() }) {
                    Ok(()) => {
                        let _ = self.reconcile();
                        format!("Project {name} created. It is now a Space in your room list; add agents with !project add {name} <agent>.")
                    }
                    Err(e) => format!("could not create {name}: {e}"),
                }
            }
            Cmd::ProjectAdd { name, agents } => self.apply(&name, &agents, true),
            Cmd::ProjectRm { name, agents } => self.apply(&name, &agents, false),
            Cmd::ProjectLead { name, agent } => match self.rt.projects() {
                Err(e) => format!("could not read projects: {e}"),
                Ok(ps) => match ps.into_iter().find(|p| p.name == name) {
                    None => format!("no project named {name}"),
                    Some(mut p) => {
                        p.lead = agent.clone();
                        match self.rt.put_project(&p) {
                            Ok(()) => match agent {
                                Some(a) => format!("{a} now answers in {name} when nobody is mentioned."),
                                None => format!("{name} has no lead: only mentioned agents answer."),
                            },
                            Err(e) => format!("could not set the lead: {e}"),
                        }
                    }
                },
            },
            Cmd::ProjectArchive { name } => self.archive_command(&name),
            Cmd::ProjectUnarchive { name } => match self.rt.unarchive_project(&name) {
                Ok(resumed) => {
                    let _ = self.reconcile();
                    format!(
                        "{name} is back: its rooms are in your list again{}.",
                        if resumed.is_empty() {
                            String::new()
                        } else {
                            format!(", and {} resumed", resumed.join(", "))
                        }
                    )
                }
                Err(e) => format!("could not unarchive {name}: {e}"),
            },
            Cmd::ProjectDelete { name } => match self.rt.delete_project(&name) {
                Ok(()) => format!("Project {name} forgotten; its agents lose its grants. The rooms stay — leave them when you like."),
                Err(e) => format!("could not delete {name}: {e}"),
            },
        };
        Some(reply)
    }

    fn apply(&self, project: &str, agents: &[String], add: bool) -> String {
        let mut done = Vec::new();
        let mut failed = Vec::new();
        for a in agents {
            let r = if add {
                self.rt.add_to_project(project, a)
            } else {
                self.rt.remove_from_project(project, a)
            };
            match r {
                Ok(()) => done.push(a.clone()),
                Err(e) => failed.push(format!("{a}: {e}")),
            }
        }
        if !done.is_empty() {
            let _ = self.reconcile();
        }
        let mut out = String::new();
        if !done.is_empty() {
            out.push_str(&format!(
                "{} {} {project}",
                done.join(", "),
                if add { "added to" } else { "removed from" }
            ));
        }
        if !failed.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&format!("failed: {}", failed.join("; ")));
        }
        out
    }

    pub fn handle_model_cmd(&self, agent: &str, new_model: Option<&str>) -> String {
        let Ok(mut val) = self.rt.agent_spec(agent) else {
            return format!("could not find agent `{agent}`");
        };
        let Some(spec) = val.get_mut("spec") else {
            return format!("could not read spec for `{agent}`");
        };
        match new_model {
            None => {
                let label = val["model_label"].as_str().unwrap_or("unknown");
                format!("Current model for `{agent}`: **{label}**")
            }
            Some(model) => {
                let model_obj = if model.contains("claude") {
                    serde_json::json!({
                        "kind": "anthropic",
                        "model": model,
                        "api_key_env": "ANTHROPIC_API_KEY"
                    })
                } else if model.contains("gemini") {
                    serde_json::json!({
                        "kind": "gemini",
                        "model": model,
                        "api_key_env": "GEMINI_API_KEY"
                    })
                } else if model == "local" {
                    serde_json::json!({ "kind": "local" })
                } else {
                    serde_json::json!({
                        "kind": "openai",
                        "model": model,
                        "base_url": "http://127.0.0.1:8000/v1",
                        "api_key_env": "OPENAI_API_KEY"
                    })
                };
                spec["model"] = model_obj;
                if let Some(sessions) = spec.get_mut("sessions") {
                    sessions["model"] = serde_json::Value::String(model.to_string());
                }
                match self.rt.update_agent_spec(agent, spec) {
                    Ok(()) => format!("Switched `{agent}` model to **{model}**."),
                    Err(e) => format!("Could not update model for `{agent}`: {e}"),
                }
            }
        }
    }

    pub fn handle_diff_cmd(&self, agent: &str) -> String {
        match self.rt.agent_diff(agent) {
            Ok(diff) if diff.trim().is_empty() => {
                format!("No uncommitted changes in workspace for `{agent}`.")
            }
            Ok(diff) => {
                format!("### Workspace diff for `{agent}`:\n```diff\n{}\n```", clip(&diff, 8000))
            }
            Err(e) => format!("Could not get diff for `{agent}`: {e}"),
        }
    }

    pub fn handle_steer_cmd(&self, agent: &str, message: &str, room: Option<&str>) -> String {
        let prompt = format!("[STEER DIRECTIVE]: {message}");
        let project = room.and_then(|r| self.project_of(r));
        let (ok, answer) = self.rt.run(agent, &prompt, "steer", None, project.as_deref(), &[]);
        if ok {
            format!(
                "✅ Steering guidance applied to `{agent}`:\n> {message}\n\n{}",
                clip(&answer, 1000)
            )
        } else {
            format!("⚠️ Steering directive failed on `{agent}`: {answer}")
        }
    }
}

#[cfg(test)]
mod trash_tests {
    use super::*;

    #[test]
    fn agent_trash_commands_parse() {
        assert_eq!(parse("!agent trash"), Some(Cmd::AgentTrash));
        assert_eq!(
            parse("!agent restore @agent-kevin-ishot:s"),
            Some(Cmd::AgentRestore { name: "kevin-ishot".into() })
        );
        assert_eq!(parse("!agent purge rower"), Some(Cmd::AgentPurge { name: "rower".into() }));
        assert!(matches!(parse("!agent restore"), Some(Cmd::Bad(_))));
        assert!(matches!(parse("!agent frobnicate"), Some(Cmd::Bad(_))));
        assert_eq!(parse("!agents"), Some(Cmd::Agents));
        assert_eq!(
            parse("!project archive rowing"),
            Some(Cmd::ProjectArchive { name: "rowing".into() })
        );
        assert_eq!(
            parse("!project unarchive rowing"),
            Some(Cmd::ProjectUnarchive { name: "rowing".into() })
        );
        assert!(matches!(parse("!project archive"), Some(Cmd::Bad(_))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_parse() {
        assert_eq!(parse("hello"), None);
        assert_eq!(parse("!help"), Some(Cmd::Help));
        assert_eq!(parse("!agents"), Some(Cmd::Agents));
        assert_eq!(parse("!project list"), Some(Cmd::ProjectList));
        assert_eq!(
            parse("!project new rowing weekly training and logs"),
            Some(Cmd::ProjectNew {
                name: "rowing".into(),
                description: "weekly training and logs".into()
            })
        );
        assert_eq!(
            parse("!project add rowing rower @agent-coach:s.net  @chef"),
            Some(Cmd::ProjectAdd {
                name: "rowing".into(),
                agents: vec!["rower".into(), "coach".into(), "chef".into()]
            })
        );
        assert_eq!(
            parse("!project rm rowing chef"),
            Some(Cmd::ProjectRm { name: "rowing".into(), agents: vec!["chef".into()] })
        );
        assert_eq!(
            parse("!project lead rowing coach"),
            Some(Cmd::ProjectLead { name: "rowing".into(), agent: Some("coach".into()) })
        );
        assert_eq!(
            parse("!project lead rowing none"),
            Some(Cmd::ProjectLead { name: "rowing".into(), agent: None })
        );
        assert_eq!(
            parse("!project delete rowing"),
            Some(Cmd::ProjectDelete { name: "rowing".into() })
        );
        assert_eq!(parse("/help"), Some(Cmd::Help));
        assert_eq!(parse("/model"), Some(Cmd::Model { agent: None, model: None }));
        assert_eq!(
            parse("/model coder"),
            Some(Cmd::Model { agent: None, model: Some("coder".into()) })
        );
        assert_eq!(
            parse("/model coder claude-3-5-sonnet"),
            Some(Cmd::Model {
                agent: Some("coder".into()),
                model: Some("claude-3-5-sonnet".into())
            })
        );
        assert_eq!(parse("/diff"), Some(Cmd::Diff { agent: None }));
        assert_eq!(parse("/diff coder"), Some(Cmd::Diff { agent: Some("coder".into()) }));
        assert_eq!(
            parse("/steer do not touch Cargo.toml"),
            Some(Cmd::Steer { agent: None, message: "do not touch Cargo.toml".into() })
        );
        assert_eq!(
            parse("/steer @coder do not touch Cargo.toml"),
            Some(Cmd::Steer {
                agent: Some("coder".into()),
                message: "do not touch Cargo.toml".into()
            })
        );
    }

    #[test]
    fn mistakes_get_a_usage_message_not_a_silent_failure() {
        for bad in [
            "!project new",
            "!project add rowing",
            "!project lead rowing",
            "!nonsense",
            "!project fly",
        ] {
            assert!(matches!(parse(bad), Some(Cmd::Bad(_))), "{bad}");
        }
        assert_eq!(parse("  !help  "), Some(Cmd::Help), "surrounding spaces are fine");
    }
}
