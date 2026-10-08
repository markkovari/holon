//! The control room's `!` commands: how projects are made and staffed from any
//! Matrix client, including ones that cannot create Spaces.

use crate::bridge::Bridge;
use crate::runtime::ProjectInfo;

#[derive(Debug, PartialEq)]
pub enum Cmd {
    Help,
    Agents,
    ProjectList,
    ProjectNew { name: String, description: String },
    ProjectAdd { name: String, agents: Vec<String> },
    ProjectRm { name: String, agents: Vec<String> },
    ProjectLead { name: String, agent: Option<String> },
    ProjectDelete { name: String },
    Bad(String),
}

/// `rower`, `@rower`, `@agent-rower:server` and `agent-rower` all mean the agent `rower`.
fn agent_token(t: &str) -> String {
    let t = t.trim_start_matches('@');
    let t = t.split(':').next().unwrap_or(t);
    t.strip_prefix("agent-").unwrap_or(t).to_string()
}

/// `None` when the text is not a command at all.
pub fn parse(body: &str) -> Option<Cmd> {
    let body = body.trim();
    if !body.starts_with('!') {
        return None;
    }
    let mut words = body[1..].split_whitespace();
    let word = |w: Option<&str>| w.unwrap_or_default().to_string();
    Some(match (words.next(), words.next()) {
        (Some("help"), _) | (Some("?"), _) => Cmd::Help,
        (Some("agents"), _) => Cmd::Agents,
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
            other => Cmd::Bad(format!("unknown project command `{other}`; try !help")),
        },
        (Some(other), _) => Cmd::Bad(format!("unknown command `!{other}`; try !help")),
        (None, _) => Cmd::Help,
    })
}

const HELP: &str = "\
!agents                                  list the agents
!project list                            list projects and who is in them
!project new <name> [description]        make a project (a Space with a general and a feed room)
!project add <name> <agent> [<agent>…]   add agents to a project
!project rm <name> <agent> [<agent>…]    take agents out
!project lead <name> <agent|none>        who answers when nobody is mentioned
!project delete <name>                   forget the project (its rooms stay)

You can also invite an agent's user (@agent-<name>) into a project's room: that adds it.";

impl Bridge {
    /// The reply to a control-room message.
    pub fn command(&self, body: &str) -> Option<String> {
        let Some(cmd) = parse(body) else {
            return Some("I only understand commands here. Try !help".into());
        };
        let reply = match cmd {
            Cmd::Help => HELP.to_string(),
            Cmd::Bad(e) => e,
            Cmd::Agents => match self.rt.agents() {
                Ok(a) if a.is_empty() => "no agents yet".into(),
                Ok(a) => a.iter().map(|x| format!("{} — {}", x.name, x.description)).collect::<Vec<_>>().join("\n"),
                Err(e) => format!("could not list agents: {e}"),
            },
            Cmd::ProjectList => match self.rt.projects() {
                Ok(p) if p.is_empty() => "no projects yet. !project new <name>".into(),
                Ok(p) => p
                    .iter()
                    .map(|p| {
                        format!(
                            "{} — {}{}",
                            p.name,
                            if p.agents.is_empty() { "(no agents)".to_string() } else { p.agents.join(", ") },
                            p.lead.as_ref().map(|l| format!("  [lead: {l}]")).unwrap_or_default()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                Err(e) => format!("could not list projects: {e}"),
            },
            Cmd::ProjectNew { name, description } => {
                match self.rt.put_project(&ProjectInfo { name: name.clone(), description, agents: vec![], lead: None }) {
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
