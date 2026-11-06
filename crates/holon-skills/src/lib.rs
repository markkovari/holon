//! Graph-native skills (ADR-0080, ADR-0091).
//! Skills are structured nodes in the capability graph with preconditions, postconditions,
//! required WIT interfaces, and composition DAG edges, projected into SurrealDB.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum SkillError {
    #[error("skill not found: {0}")]
    NotFound(String),
    #[error("cycle detected in skill graph involving: {0}")]
    CycleDetected(String),
    #[error("missing required capability/interface: {0}")]
    MissingPrerequisite(String),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillNode {
    pub id: String,
    pub name: String,
    pub intent: String,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub postconditions: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillEdge {
    RequiresInterface(String),
    ComposesSkill(String),
    CarriesArtifact(String),
    ProvenBy(String),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SkillGraph {
    pub nodes: BTreeMap<String, SkillNode>,
    pub edges: BTreeMap<String, BTreeSet<SkillEdge>>,
}

impl SkillGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert_node(&mut self, node: SkillNode) {
        let id = node.id.clone();
        self.nodes.insert(id.clone(), node);
        self.edges.entry(id).or_default();
    }

    pub fn add_edge(&mut self, from_id: &str, edge: SkillEdge) -> Result<(), SkillError> {
        if !self.nodes.contains_key(from_id) {
            return Err(SkillError::NotFound(from_id.to_string()));
        }
        if let SkillEdge::ComposesSkill(ref target_id) = edge {
            if !self.nodes.contains_key(target_id) {
                return Err(SkillError::NotFound(target_id.clone()));
            }
            if from_id == target_id {
                return Err(SkillError::CycleDetected(from_id.to_string()));
            }
        }
        self.edges.entry(from_id.to_string()).or_default().insert(edge);
        Ok(())
    }

    pub fn get_node(&self, id: &str) -> Option<&SkillNode> {
        self.nodes.get(id)
    }

    pub fn edges_from(&self, id: &str) -> Option<&BTreeSet<SkillEdge>> {
        self.edges.get(id)
    }

    /// Topological sort of composed sub-skills to guarantee dependency ordering.
    pub fn resolve_subskills(&self, root_id: &str) -> Result<Vec<SkillNode>, SkillError> {
        if !self.nodes.contains_key(root_id) {
            return Err(SkillError::NotFound(root_id.to_string()));
        }

        let mut ordered = Vec::new();
        let mut visited = BTreeSet::new();
        let mut visiting = BTreeSet::new();

        self.dfs_subskills(root_id, &mut visiting, &mut visited, &mut ordered)?;
        Ok(ordered)
    }

    fn dfs_subskills(
        &self,
        node_id: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<SkillNode>,
    ) -> Result<(), SkillError> {
        if visiting.contains(node_id) {
            return Err(SkillError::CycleDetected(node_id.to_string()));
        }
        if visited.contains(node_id) {
            return Ok(());
        }

        visiting.insert(node_id.to_string());

        if let Some(edges) = self.edges.get(node_id) {
            for edge in edges {
                if let SkillEdge::ComposesSkill(ref child) = edge {
                    self.dfs_subskills(child, visiting, visited, ordered)?;
                }
            }
        }

        visiting.remove(node_id);
        visited.insert(node_id.to_string());

        if let Some(node) = self.nodes.get(node_id) {
            ordered.push(node.clone());
        }
        Ok(())
    }

    /// Collect all WIT interface and capability dependencies across the skill hierarchy.
    pub fn required_interfaces(&self, root_id: &str) -> Result<BTreeSet<String>, SkillError> {
        let skills = self.resolve_subskills(root_id)?;
        let mut ifaces = BTreeSet::new();
        for s in skills {
            if let Some(edges) = self.edges.get(&s.id) {
                for edge in edges {
                    if let SkillEdge::RequiresInterface(ref iface) = edge {
                        ifaces.insert(iface.clone());
                    }
                }
            }
        }
        Ok(ifaces)
    }

    /// Check if the provided capabilities satisfy the required interfaces of the skill DAG.
    pub fn can_execute(&self, root_id: &str, available_capabilities: &[String]) -> bool {
        let Ok(required) = self.required_interfaces(root_id) else {
            return false;
        };
        let available: BTreeSet<&str> = available_capabilities.iter().map(String::as_str).collect();
        required.iter().all(|req| available.contains(req.as_str()))
    }

    /// Find skills matching a search query over intent, name, or tags.
    pub fn search(&self, query: &str) -> Vec<SkillNode> {
        let q = query.to_lowercase();
        self.nodes
            .values()
            .filter(|n| {
                n.name.to_lowercase().contains(&q)
                    || n.intent.to_lowercase().contains(&q)
                    || n.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .cloned()
            .collect()
    }

    /// SurrealQL projection statements for `comp-capgraph` (ADR-0091).
    pub fn to_surql(&self, generation: u64) -> String {
        let mut out = String::new();
        out.push_str("DEFINE TABLE IF NOT EXISTS skill;\n");
        out.push_str("DEFINE TABLE IF NOT EXISTS requires TYPE RELATION;\n");
        out.push_str("DEFINE TABLE IF NOT EXISTS composes TYPE RELATION;\n");
        out.push_str("DEFINE TABLE IF NOT EXISTS implements TYPE RELATION;\n");

        for (id, node) in &self.nodes {
            let rid_node = format!("skill:⟨{}⟩", id.replace('⟩', ""));
            let name_lit = serde_json::to_string(&node.name).unwrap_or_default();
            let intent_lit = serde_json::to_string(&node.intent).unwrap_or_default();
            let pre_lit = serde_json::to_string(&node.preconditions).unwrap_or_default();
            let post_lit = serde_json::to_string(&node.postconditions).unwrap_or_default();
            let tags_lit = serde_json::to_string(&node.tags).unwrap_or_default();

            out.push_str(&format!(
                "UPSERT {rid_node} SET name = {name_lit}, intent = {intent_lit}, preconditions = {pre_lit}, postconditions = {post_lit}, tags = {tags_lit}, gen = {generation};\n"
            ));

            if let Some(edges) = self.edges.get(id) {
                for edge in edges {
                    match edge {
                        SkillEdge::RequiresInterface(iface) => {
                            let rid_iface = format!("interface:⟨{}⟩", iface.replace('⟩', ""));
                            out.push_str(&format!(
                                "RELATE {rid_node}->requires->{rid_iface} SET gen = {generation};\n"
                            ));
                        }
                        SkillEdge::ComposesSkill(sub) => {
                            let rid_sub = format!("skill:⟨{}⟩", sub.replace('⟩', ""));
                            out.push_str(&format!(
                                "RELATE {rid_node}->composes->{rid_sub} SET gen = {generation};\n"
                            ));
                        }
                        SkillEdge::CarriesArtifact(art) => {
                            let rid_art = format!("artifact:⟨{}⟩", art.replace('⟩', ""));
                            out.push_str(&format!(
                                "RELATE {rid_art}->implements->{rid_node} SET gen = {generation};\n"
                            ));
                        }
                        SkillEdge::ProvenBy(_) => {}
                    }
                }
            }
        }
        out
    }

    /// Built-in foundational skills library.
    pub fn default_skills() -> Self {
        let mut g = Self::new();

        g.insert_node(SkillNode {
            id: "git:propose_pr".to_string(),
            name: "Propose Git Pull Request".to_string(),
            intent: "Create an isolated git branch, commit modified files, and open a PR with graph provenance".to_string(),
            preconditions: vec!["workspace:files_modified".to_string()],
            postconditions: vec!["git:pr_opened".to_string(), "graph:pr_recorded".to_string()],
            tags: vec!["git".to_string(), "forge".to_string(), "pr".to_string()],
        });
        let _ = g.add_edge(
            "git:propose_pr",
            SkillEdge::RequiresInterface("git:forge/repo@0.1.0".to_string()),
        );
        let _ =
            g.add_edge("git:propose_pr", SkillEdge::CarriesArtifact("github-forge".to_string()));

        g.insert_node(SkillNode {
            id: "wit:scaffold_interface".to_string(),
            name: "Scaffold WIT Interface".to_string(),
            intent: "Generate WIT world and interface contracts for wasm32-wasip2 components"
                .to_string(),
            preconditions: vec!["contract:defined".to_string()],
            postconditions: vec!["wit:files_written".to_string()],
            tags: vec!["wit".to_string(), "wasm".to_string(), "contract".to_string()],
        });

        g.insert_node(SkillNode {
            id: "rust:compile_check".to_string(),
            name: "Rust Compile Check".to_string(),
            intent: "Compile candidate Rust packages and assert strict type safety and clippy compliance".to_string(),
            preconditions: vec!["rust:files_present".to_string()],
            postconditions: vec!["rust:build_passed".to_string()],
            tags: vec!["cargo".to_string(), "rust".to_string(), "gate".to_string()],
        });

        g.insert_node(SkillNode {
            id: "gate:evaluate".to_string(),
            name: "Evaluate Candidate Gate".to_string(),
            intent:
                "Run all required gate check suites, benchmark thresholds, and fitness assertions"
                    .to_string(),
            preconditions: vec!["candidate:compiled".to_string()],
            postconditions: vec!["gate:score_computed".to_string()],
            tags: vec!["gate".to_string(), "fitness".to_string(), "eval".to_string()],
        });

        g.insert_node(SkillNode {
            id: "code:generate_batch".to_string(),
            name: "Free Tier Batch Code Generation".to_string(),
            intent:
                "Generate code candidate via NATS JetStream coalesced batch queue at $0.00 spend"
                    .to_string(),
            preconditions: vec!["prompt:spec_ready".to_string()],
            postconditions: vec!["code:candidate_generated".to_string()],
            tags: vec!["llm".to_string(), "batch".to_string(), "free-tier".to_string()],
        });

        g.insert_node(SkillNode {
            id: "harness:measure".to_string(),
            name: "Measure Harness Effectiveness".to_string(),
            intent: "Analyze failure distributions, catalog component reuse, and generation yield"
                .to_string(),
            preconditions: vec!["runs:recorded".to_string()],
            postconditions: vec!["metrics:computed".to_string()],
            tags: vec!["metrics".to_string(), "harness".to_string(), "reuse".to_string()],
        });

        // Composed skill: Autonomous Agent Feature Delivery
        g.insert_node(SkillNode {
            id: "agent:deliver_feature".to_string(),
            name: "Autonomous Feature Delivery".to_string(),
            intent: "Generate code on free tier batch queue, compile, evaluate gate checks, and propose PR".to_string(),
            preconditions: vec!["goal:specified".to_string()],
            postconditions: vec!["feature:landed_or_pr_opened".to_string()],
            tags: vec!["agent".to_string(), "swarm".to_string(), "autonomous".to_string()],
        });
        let _ = g.add_edge(
            "agent:deliver_feature",
            SkillEdge::ComposesSkill("code:generate_batch".to_string()),
        );
        let _ = g.add_edge(
            "agent:deliver_feature",
            SkillEdge::ComposesSkill("rust:compile_check".to_string()),
        );
        let _ = g.add_edge(
            "agent:deliver_feature",
            SkillEdge::ComposesSkill("gate:evaluate".to_string()),
        );
        let _ = g.add_edge(
            "agent:deliver_feature",
            SkillEdge::ComposesSkill("git:propose_pr".to_string()),
        );

        g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_skills_resolve_dependencies_in_dag() {
        let g = SkillGraph::default_skills();
        let sub = g.resolve_subskills("agent:deliver_feature").unwrap();
        let ids: Vec<&str> = sub.iter().map(|n| n.id.as_str()).collect();
        assert!(ids.contains(&"code:generate_batch"));
        assert!(ids.contains(&"rust:compile_check"));
        assert!(ids.contains(&"gate:evaluate"));
        assert!(ids.contains(&"git:propose_pr"));
        assert_eq!(*ids.last().unwrap(), "agent:deliver_feature");
    }

    #[test]
    fn required_interfaces_aggregates_across_dag() {
        let g = SkillGraph::default_skills();
        let ifaces = g.required_interfaces("agent:deliver_feature").unwrap();
        assert!(ifaces.contains("git:forge/repo@0.1.0"));
    }

    #[test]
    fn can_execute_verifies_required_capabilities() {
        let g = SkillGraph::default_skills();
        assert!(!g.can_execute("agent:deliver_feature", &[]));
        assert!(g.can_execute("agent:deliver_feature", &["git:forge/repo@0.1.0".to_string()]));
    }

    #[test]
    fn cycle_detection_returns_error() {
        let mut g = SkillGraph::new();
        g.insert_node(SkillNode {
            id: "a".to_string(),
            name: "A".to_string(),
            intent: "A".to_string(),
            preconditions: vec![],
            postconditions: vec![],
            tags: vec![],
        });
        g.insert_node(SkillNode {
            id: "b".to_string(),
            name: "B".to_string(),
            intent: "B".to_string(),
            preconditions: vec![],
            postconditions: vec![],
            tags: vec![],
        });
        g.add_edge("a", SkillEdge::ComposesSkill("b".to_string())).unwrap();
        g.add_edge("b", SkillEdge::ComposesSkill("a".to_string())).unwrap();

        assert!(matches!(g.resolve_subskills("a"), Err(SkillError::CycleDetected(_))));
    }

    #[test]
    fn to_surql_generates_valid_table_and_relate_statements() {
        let g = SkillGraph::default_skills();
        let surql = g.to_surql(1);
        assert!(surql.contains("DEFINE TABLE IF NOT EXISTS skill;"));
        assert!(surql.contains("skill:⟨git:propose_pr⟩"));
        assert!(surql
            .contains("RELATE skill:⟨git:propose_pr⟩->requires->interface:⟨git:forge/repo@0.1.0⟩"));
    }
}
