//! Projects: a named group of agents working on something together
//! ("rowing", "nutrition"). Membership IS the grant.
//!
//! An agent that is a member of project `rowing` may, without its own spec
//! saying so, use the shared store namespace `project.rowing` and publish to
//! topics `rowing.*`, and its prompt says which project it is working in and who
//! else is in it. Removing it from the project takes all of that away at its
//! next run. So "add an agent to a project" is one action, wherever it is
//! performed from (the admin API, the console, or inviting the agent's user to a
//! Matrix space), and cannot drift from what the agent is actually allowed to do.
//!
//! The runtime's registry is the source of truth. A chat bridge is a view of it.

use serde::{Deserialize, Serialize};

use crate::spec::{validate_name, AgentSpec, Capability};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct Project {
    /// Lowercase words joined by dashes. It is also the topic prefix and part of
    /// the store namespace, so it has the same rules as an agent name.
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub agents: Vec<String>,
    /// The member that answers when a message in the project does not address
    /// anyone in particular. `None`: nobody does, and agents answer only when
    /// addressed. Must be a member.
    #[serde(default)]
    pub lead: Option<String>,
}

impl Project {
    /// The shared store namespace every member may read and write.
    pub fn store_ns(&self) -> String {
        format!("project.{}", self.name)
    }

    /// Members may emit events to `<name>.*`.
    pub fn topic_glob(&self) -> String {
        format!("{}.*", self.name)
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.name)?;
        for a in &self.agents {
            validate_name(a)?;
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(dup) = self.agents.iter().find(|a| !seen.insert(*a)) {
            return Err(format!("`{dup}` is listed twice"));
        }
        if let Some(lead) = &self.lead {
            if !self.agents.contains(lead) {
                return Err(format!("lead `{lead}` is not a member of the project"));
            }
        }
        Ok(())
    }
}

/// What an agent knows about a project it belongs to. Filled in at run time
/// from the registry; never written into the agent's spec file.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ProjectInfo {
    pub name: String,
    pub description: String,
    pub members: Vec<String>,
    pub lead: Option<String>,
    pub store_ns: String,
    pub topic_glob: String,
}

/// `spec` with the grants its project memberships imply. The stored spec is
/// untouched; this is what a run actually uses.
pub fn effective(spec: &AgentSpec, projects: &[Project]) -> AgentSpec {
    let mut s = spec.clone();
    for p in projects.iter().filter(|p| p.agents.contains(&spec.name)) {
        let ns = p.store_ns();
        if !s.store.write.contains(&ns) {
            s.store.write.push(ns.clone());
        }
        let glob = p.topic_glob();
        if !s.topics_out.contains(&glob) {
            s.topics_out.push(glob.clone());
        }
        // The grants are useless without the tools that use them.
        for tool in ["store_get", "store_put", "store_list", "emit_event"] {
            if !s.has_capability(tool) {
                s.capabilities.push(Capability::named(tool));
            }
        }
        s.projects.push(ProjectInfo {
            name: p.name.clone(),
            description: p.description.clone(),
            members: p.agents.clone(),
            lead: p.lead.clone(),
            store_ns: ns,
            topic_glob: glob,
        });
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, agents: &[&str]) -> Project {
        Project {
            name: name.into(),
            description: "training".into(),
            agents: agents.iter().map(|a| a.to_string()).collect(),
            lead: None,
        }
    }

    #[test]
    fn membership_grants_the_namespace_the_topics_and_the_tools() {
        let spec = AgentSpec::new("coach", "coaches");
        assert!(!spec.has_capability("store_put") && spec.store.write.is_empty());
        let eff = effective(&spec, &[project("rowing", &["coach", "rower"])]);
        assert_eq!(eff.store.write, ["project.rowing"]);
        assert_eq!(eff.topics_out, ["rowing.*"]);
        for t in ["store_get", "store_put", "store_list", "emit_event"] {
            assert!(eff.has_capability(t), "{t}");
        }
        assert_eq!(eff.projects[0].members, ["coach", "rower"]);
        // the stored spec is untouched
        assert!(spec.store.write.is_empty() && spec.topics_out.is_empty());
    }

    #[test]
    fn non_members_get_nothing_and_grants_are_not_duplicated() {
        let spec = AgentSpec::new("other", "x");
        assert_eq!(effective(&spec, &[project("rowing", &["coach"])]), spec);
        let mut own = AgentSpec::new("coach", "x");
        own.store.write.push("project.rowing".into());
        own.capabilities.push(Capability::named("store_put"));
        let eff = effective(&own, &[project("rowing", &["coach"])]);
        assert_eq!(eff.store.write, ["project.rowing"]);
        assert_eq!(eff.capabilities.iter().filter(|c| c.name == "store_put").count(), 1);
    }

    #[test]
    fn several_projects_stack() {
        let spec = AgentSpec::new("coach", "x");
        let eff = effective(
            &spec,
            &[project("rowing", &["coach"]), project("nutrition", &["coach", "chef"])],
        );
        assert_eq!(eff.store.write, ["project.rowing", "project.nutrition"]);
        assert_eq!(eff.topics_out, ["rowing.*", "nutrition.*"]);
        assert_eq!(eff.projects.len(), 2);
    }

    #[test]
    fn validation() {
        assert!(project("rowing", &["a", "b"]).validate().is_ok());
        assert!(project("Rowing", &[]).validate().is_err());
        assert!(project("rowing", &["a", "a"]).validate().unwrap_err().contains("twice"));
        assert!(project("rowing", &["../x"]).validate().is_err());
        let mut p = project("rowing", &["a"]);
        p.lead = Some("b".into());
        assert!(p.validate().unwrap_err().contains("not a member"));
        p.lead = Some("a".into());
        assert!(p.validate().is_ok());
    }
}
