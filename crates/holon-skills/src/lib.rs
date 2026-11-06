//! Graph-native skills (ADR-0080, ADR-0091).
//! Skills are structured nodes in the capability graph with preconditions, postconditions,
//! required WIT interfaces, and composition DAG edges, projected into SurrealDB.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum SkillError {
    #[error("skill not found: {0}")]
    NotFound(String),
    #[error("cycle detected in skill graph involving: {0}")]
    CycleDetected(String),
    #[error("missing required capability/interface: {0}")]
    MissingPrerequisite(String),
    #[error("failed to parse skill definition: {0}")]
    ParseError(String),
    #[error("filesystem io error: {0}")]
    IoError(String),
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
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
pub enum SkillEdge {
    RequiresInterface(String),
    ComposesSkill(String),
    CarriesArtifact(String),
    ProvenBy(String),
}

/// Declarative skill definition format (for .toml / .json files).
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillDefinition {
    pub id: String,
    pub name: String,
    pub intent: String,
    #[serde(default)]
    pub preconditions: Vec<String>,
    #[serde(default)]
    pub postconditions: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub composes: Vec<String>,
    #[serde(default)]
    pub carries: Vec<String>,
}

/// Overlap, bloat, or divergence issues identified during registry audit.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SkillAuditIssue {
    Duplicate {
        skill_a: String,
        skill_b: String,
        similarity_pct: u8,
        reason: String,
    },
    Monolithic {
        skill_id: String,
        step_count: usize,
        distinct_capabilities: usize,
        recommendation: String,
    },
    DivergentStepOverlap {
        skill_a: String,
        skill_b: String,
        shared_steps: Vec<String>,
        recommendation: String,
    },
    MissingPrerequisite {
        skill_id: String,
        missing_target: String,
    },
}

/// Execution plan generated when a skill is activated.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkillPlan {
    pub root_skill: SkillNode,
    pub steps: Vec<SkillNode>,
    pub required_interfaces: BTreeSet<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct SkillGraph {
    pub nodes: BTreeMap<String, SkillNode>,
    pub edges: BTreeMap<String, BTreeSet<SkillEdge>>,
    #[serde(default)]
    pub aliases: BTreeMap<String, String>,
}

impl SkillGraph {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn canonical_id<'a>(&'a self, id: &'a str) -> &'a str {
        self.aliases.get(id).map(String::as_str).unwrap_or(id)
    }

    pub fn insert_node(&mut self, node: SkillNode) {
        let id = node.id.clone();
        for alias in &node.aliases {
            self.aliases.insert(alias.clone(), id.clone());
        }
        self.nodes.insert(id.clone(), node);
        self.edges.entry(id).or_default();
    }

    pub fn add_edge(&mut self, from_id: &str, edge: SkillEdge) -> Result<(), SkillError> {
        let from_canonical = self.canonical_id(from_id);
        if !self.nodes.contains_key(from_canonical) {
            return Err(SkillError::NotFound(from_id.to_string()));
        }
        let stored_edge = match edge {
            SkillEdge::ComposesSkill(ref target_id) => {
                let target_canonical = self.canonical_id(target_id);
                if !self.nodes.contains_key(target_canonical) {
                    return Err(SkillError::NotFound(target_id.clone()));
                }
                if from_canonical == target_canonical {
                    return Err(SkillError::CycleDetected(from_id.to_string()));
                }
                SkillEdge::ComposesSkill(target_canonical.to_string())
            }
            other => other,
        };
        self.edges.entry(from_canonical.to_string()).or_default().insert(stored_edge);
        Ok(())
    }

    pub fn register_definition(&mut self, def: SkillDefinition) -> Result<(), SkillError> {
        let id = def.id.clone();
        self.insert_node(SkillNode {
            id: id.clone(),
            name: def.name,
            intent: def.intent,
            preconditions: def.preconditions,
            postconditions: def.postconditions,
            tags: def.tags,
            aliases: def.aliases,
        });

        for req in def.requires {
            self.add_edge(&id, SkillEdge::RequiresInterface(req))?;
        }
        for sub in def.composes {
            self.add_edge(&id, SkillEdge::ComposesSkill(sub))?;
        }
        for art in def.carries {
            self.add_edge(&id, SkillEdge::CarriesArtifact(art))?;
        }
        Ok(())
    }

    /// Load a skill from declarative TOML string.
    pub fn from_toml(content: &str) -> Result<Self, SkillError> {
        let def: SkillDefinition =
            toml::from_str(content).map_err(|e| SkillError::ParseError(e.to_string()))?;
        let mut g = Self::new();
        g.register_definition(def)?;
        Ok(g)
    }

    /// Load all `.toml` skills from a directory.
    pub fn load_dir(&mut self, dir: &Path) -> Result<usize, SkillError> {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return Ok(0);
        };
        let mut loaded = 0;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "toml") {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    if let Ok(def) = toml::from_str::<SkillDefinition>(&content) {
                        if self.register_definition(def).is_ok() {
                            loaded += 1;
                        }
                    }
                }
            }
        }
        Ok(loaded)
    }

    pub fn get_node(&self, id: &str) -> Option<&SkillNode> {
        self.nodes.get(self.canonical_id(id))
    }

    pub fn edges_from(&self, id: &str) -> Option<&BTreeSet<SkillEdge>> {
        self.edges.get(self.canonical_id(id))
    }

    /// Topological sort of composed sub-skills to guarantee dependency ordering.
    pub fn resolve_subskills(&self, root_id: &str) -> Result<Vec<SkillNode>, SkillError> {
        let canonical = self.canonical_id(root_id);
        if !self.nodes.contains_key(canonical) {
            return Err(SkillError::NotFound(root_id.to_string()));
        }

        let mut ordered = Vec::new();
        let mut visited = BTreeSet::new();
        let mut visiting = BTreeSet::new();

        self.dfs_subskills(canonical, &mut visiting, &mut visited, &mut ordered)?;
        Ok(ordered)
    }

    fn dfs_subskills(
        &self,
        node_id: &str,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        ordered: &mut Vec<SkillNode>,
    ) -> Result<(), SkillError> {
        let canonical = self.canonical_id(node_id);
        if visiting.contains(canonical) {
            return Err(SkillError::CycleDetected(canonical.to_string()));
        }
        if visited.contains(canonical) {
            return Ok(());
        }

        visiting.insert(canonical.to_string());

        if let Some(edges) = self.edges.get(canonical) {
            for edge in edges {
                if let SkillEdge::ComposesSkill(ref child) = edge {
                    self.dfs_subskills(child, visiting, visited, ordered)?;
                }
            }
        }

        visiting.remove(canonical);
        visited.insert(canonical.to_string());

        if let Some(node) = self.nodes.get(canonical) {
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

    /// Generate an activation plan: ordered subskills and required WIT interfaces.
    pub fn plan(&self, root_id: &str) -> Result<SkillPlan, SkillError> {
        let canonical = self.canonical_id(root_id);
        let root = self
            .get_node(canonical)
            .cloned()
            .ok_or_else(|| SkillError::NotFound(root_id.to_string()))?;
        let steps = self.resolve_subskills(canonical)?;
        let required_interfaces = self.required_interfaces(canonical)?;
        Ok(SkillPlan { root_skill: root, steps, required_interfaces })
    }

    /// Check if the provided capabilities satisfy the required interfaces of the skill DAG.
    pub fn can_execute(&self, root_id: &str, available_capabilities: &[String]) -> bool {
        let canonical = self.canonical_id(root_id);
        let Ok(required) = self.required_interfaces(canonical) else {
            return false;
        };
        let available: BTreeSet<&str> = available_capabilities.iter().map(String::as_str).collect();
        required.iter().all(|req| available.contains(req.as_str()))
    }

    /// Audit registry for duplicates, bloated monolithic skills, and broken prerequisites.
    pub fn audit(&self) -> Vec<SkillAuditIssue> {
        let mut issues = Vec::new();

        // Check for missing prerequisites and monolithic skills
        for id in self.nodes.keys() {
            if let Some(edges) = self.edges.get(id) {
                for edge in edges {
                    if let SkillEdge::ComposesSkill(ref child) = edge {
                        if !self.nodes.contains_key(child) && !self.aliases.contains_key(child) {
                            issues.push(SkillAuditIssue::MissingPrerequisite {
                                skill_id: id.clone(),
                                missing_target: child.clone(),
                            });
                        }
                    }
                }
            }

            if let Ok(steps) = self.resolve_subskills(id) {
                let distinct_interfaces = self.required_interfaces(id).unwrap_or_default();
                if steps.len() > 6 || (distinct_interfaces.len() > 4 && steps.len() <= 1) {
                    issues.push(SkillAuditIssue::Monolithic {
                        skill_id: id.clone(),
                        step_count: steps.len(),
                        distinct_capabilities: distinct_interfaces.len(),
                        recommendation: format!(
                            "Skill '{id}' has {} steps and requires {} distinct capabilities; decompose into atomic sub-skills and compose via composes edges",
                            steps.len(),
                            distinct_interfaces.len()
                        ),
                    });
                }
            }
        }

        // Pairwise comparison for duplicates and divergent step overlaps
        let node_list: Vec<&SkillNode> = self.nodes.values().collect();
        for i in 0..node_list.len() {
            for j in (i + 1)..node_list.len() {
                let a = node_list[i];
                let b = node_list[j];

                if self.canonical_id(&a.id) == self.canonical_id(&b.id) {
                    continue;
                }

                let req_a = self.required_interfaces(&a.id).unwrap_or_default();
                let req_b = self.required_interfaces(&b.id).unwrap_or_default();

                let req_intersection = req_a.intersection(&req_b).count();
                let req_union = req_a.union(&req_b).count();
                let req_jaccard =
                    if req_union == 0 { 0.0 } else { req_intersection as f64 / req_union as f64 };

                let tokens_a = tokenize_skill(a);
                let tokens_b = tokenize_skill(b);
                let tok_intersection = tokens_a.intersection(&tokens_b).count();
                let tok_union = tokens_a.union(&tokens_b).count();
                let tok_jaccard =
                    if tok_union == 0 { 0.0 } else { tok_intersection as f64 / tok_union as f64 };

                if (req_jaccard >= 0.70 && tok_jaccard >= 0.40)
                    || (req_jaccard == 1.0 && req_union > 0 && tok_jaccard >= 0.35)
                {
                    let score = ((req_jaccard * 0.5 + tok_jaccard * 0.5) * 100.0) as u8;
                    issues.push(SkillAuditIssue::Duplicate {
                        skill_a: a.id.clone(),
                        skill_b: b.id.clone(),
                        similarity_pct: score,
                        reason: format!(
                            "Skills '{}' and '{}' share {:.0}% capability and {:.0}% intent token overlap; recommend consolidating under one canonical skill with aliases",
                            a.id,
                            b.id,
                            req_jaccard * 100.0,
                            tok_jaccard * 100.0
                        ),
                    });
                }

                if let (Ok(steps_a), Ok(steps_b)) =
                    (self.resolve_subskills(&a.id), self.resolve_subskills(&b.id))
                {
                    if steps_a.len() >= 3 && steps_b.len() >= 3 {
                        let step_set_a: BTreeSet<&str> =
                            steps_a.iter().map(|s| s.id.as_str()).filter(|s| *s != a.id).collect();
                        let step_set_b: BTreeSet<&str> =
                            steps_b.iter().map(|s| s.id.as_str()).filter(|s| *s != b.id).collect();
                        let shared: Vec<String> =
                            step_set_a.intersection(&step_set_b).map(|s| s.to_string()).collect();
                        if shared.len() >= 2 && step_set_a != step_set_b {
                            issues.push(SkillAuditIssue::DivergentStepOverlap {
                                skill_a: a.id.clone(),
                                skill_b: b.id.clone(),
                                shared_steps: shared.clone(),
                                recommendation: format!(
                                    "Skills '{}' and '{}' share steps {shared:?}; consider extracting shared steps into a common base skill",
                                    a.id, b.id
                                ),
                            });
                        }
                    }
                }
            }
        }

        issues
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

    /// Built-in foundational skills library including contract-first scaffolding and bug repair.
    pub fn default_skills() -> Self {
        let mut g = Self::new();

        g.insert_node(SkillNode {
            id: "git:propose_pr".to_string(),
            name: "Propose Git Pull Request".to_string(),
            intent: "Create an isolated git branch, commit modified files, and open a PR with graph provenance".to_string(),
            preconditions: vec!["workspace:files_modified".to_string()],
            postconditions: vec!["git:pr_opened".to_string(), "graph:pr_recorded".to_string()],
            tags: vec!["git".to_string(), "forge".to_string(), "pr".to_string()],
            aliases: vec!["git:create_pull_request".to_string()],
        });
        let _ = g.add_edge(
            "git:propose_pr",
            SkillEdge::RequiresInterface("git:forge/repo@0.1.0".to_string()),
        );
        let _ =
            g.add_edge("git:propose_pr", SkillEdge::CarriesArtifact("github-forge".to_string()));

        g.insert_node(SkillNode {
            id: "rust:compile_check".to_string(),
            name: "Rust Compile Check".to_string(),
            intent: "Compile candidate Rust packages and assert strict type safety and clippy compliance".to_string(),
            preconditions: vec!["rust:files_present".to_string()],
            postconditions: vec!["rust:build_passed".to_string()],
            tags: vec!["cargo".to_string(), "rust".to_string(), "gate".to_string()],
            aliases: vec![],
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
            aliases: vec![],
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
            aliases: vec![],
        });

        g.insert_node(SkillNode {
            id: "harness:measure".to_string(),
            name: "Measure Harness Effectiveness".to_string(),
            intent: "Analyze failure distributions, catalog component reuse, and generation yield"
                .to_string(),
            preconditions: vec!["runs:recorded".to_string()],
            postconditions: vec!["metrics:computed".to_string()],
            tags: vec!["metrics".to_string(), "harness".to_string(), "reuse".to_string()],
            aliases: vec![],
        });

        // ---------------------------------------------------------------------------------
        // Skill 1: WIT Contract-First Scaffolder (wit:contract_first_scaffold)
        // ---------------------------------------------------------------------------------
        g.insert_node(SkillNode {
            id: "wit:draft_interface".to_string(),
            name: "Draft WIT Interface".to_string(),
            intent: "Draft WIT world and interface definitions (.wit) with types and functions"
                .to_string(),
            preconditions: vec!["goal:feature_spec_defined".to_string()],
            postconditions: vec!["wit:files_written".to_string()],
            tags: vec!["wit".to_string(), "contract".to_string()],
            aliases: vec![],
        });
        g.insert_node(SkillNode {
            id: "wit:validate_ast".to_string(),
            name: "Validate WIT AST".to_string(),
            intent: "Verify WIT AST syntax validity and semver safety".to_string(),
            preconditions: vec!["wit:files_written".to_string()],
            postconditions: vec!["wit:interface_validated".to_string()],
            tags: vec!["wit".to_string(), "validator".to_string()],
            aliases: vec![],
        });
        g.insert_node(SkillNode {
            id: "bindings:generate_xtask".to_string(),
            name: "Generate Guest Bindings".to_string(),
            intent: "Execute cargo xtask bindings to produce guest Rust glue".to_string(),
            preconditions: vec!["wit:interface_validated".to_string()],
            postconditions: vec!["bindings:generated".to_string()],
            tags: vec!["xtask".to_string(), "wasm".to_string()],
            aliases: vec![],
        });
        g.insert_node(SkillNode {
            id: "component:scaffold_crate".to_string(),
            name: "Scaffold Component Crate".to_string(),
            intent: "Create Cargo.toml and src/lib.rs guest skeleton implementing world export"
                .to_string(),
            preconditions: vec!["bindings:generated".to_string()],
            postconditions: vec!["component:cargo_scaffolded".to_string()],
            tags: vec!["cargo".to_string(), "scaffold".to_string()],
            aliases: vec![],
        });

        g.insert_node(SkillNode {
            id: "wit:contract_first_scaffold".to_string(),
            name: "WIT Contract-First Component Scaffolder".to_string(),
            intent: "Scaffold a new WASM component by writing WIT contracts first, validating syntax, generating bindings, and creating crate skeleton".to_string(),
            preconditions: vec!["goal:feature_spec_defined".to_string()],
            postconditions: vec!["wit:interface_validated".to_string(), "bindings:generated".to_string(), "component:cargo_scaffolded".to_string()],
            tags: vec!["wit".to_string(), "wasm".to_string(), "contract".to_string(), "scaffold".to_string()],
            aliases: vec!["wit:scaffold_component".to_string()],
        });
        let _ = g.add_edge(
            "wit:contract_first_scaffold",
            SkillEdge::RequiresInterface("wit:reflect@0.1.0".to_string()),
        );
        let _ = g.add_edge(
            "wit:contract_first_scaffold",
            SkillEdge::RequiresInterface("wasi:filesystem".to_string()),
        );
        let _ = g.add_edge(
            "wit:contract_first_scaffold",
            SkillEdge::CarriesArtifact("wit-reflect".to_string()),
        );
        let _ = g.add_edge(
            "wit:contract_first_scaffold",
            SkillEdge::ComposesSkill("wit:draft_interface".to_string()),
        );
        let _ = g.add_edge(
            "wit:contract_first_scaffold",
            SkillEdge::ComposesSkill("wit:validate_ast".to_string()),
        );
        let _ = g.add_edge(
            "wit:contract_first_scaffold",
            SkillEdge::ComposesSkill("bindings:generate_xtask".to_string()),
        );
        let _ = g.add_edge(
            "wit:contract_first_scaffold",
            SkillEdge::ComposesSkill("component:scaffold_crate".to_string()),
        );

        // ---------------------------------------------------------------------------------
        // Skill 2: Autonomous Bug Repro & Gate-Driven Repair (bug:autonomous_repro_and_repair)
        // ---------------------------------------------------------------------------------
        g.insert_node(SkillNode {
            id: "test:author_failing_gate".to_string(),
            name: "Author Failing Gate Test".to_string(),
            intent: "Write a gate test case reproducing the reported bug in tests/".to_string(),
            preconditions: vec!["bug:report_given".to_string()],
            postconditions: vec!["test:code_written".to_string()],
            tags: vec!["gate".to_string(), "test".to_string()],
            aliases: vec![],
        });
        g.insert_node(SkillNode {
            id: "gate:prove_initial_failure".to_string(),
            name: "Prove Initial Gate Failure".to_string(),
            intent: "Run gate test against baseline to verify that it fails before any repair is applied".to_string(),
            preconditions: vec!["test:code_written".to_string()],
            postconditions: vec!["test:pre_failure_proven".to_string()],
            tags: vec!["gate".to_string(), "repro".to_string()],
            aliases: vec![],
        });
        g.insert_node(SkillNode {
            id: "code:generate_repair".to_string(),
            name: "Generate Code Repair".to_string(),
            intent: "Submit bug repair prompt to free-tier queue and apply candidate patch"
                .to_string(),
            preconditions: vec!["test:pre_failure_proven".to_string()],
            postconditions: vec!["patch:applied".to_string()],
            tags: vec!["repair".to_string(), "llm".to_string()],
            aliases: vec![],
        });
        g.insert_node(SkillNode {
            id: "gate:verify_green".to_string(),
            name: "Verify Gate All Green".to_string(),
            intent: "Execute full gate check suite and verify all assertions pass with 100% green exit code 0".to_string(),
            preconditions: vec!["patch:applied".to_string()],
            postconditions: vec!["gate:all_passing".to_string()],
            tags: vec!["gate".to_string(), "verify".to_string()],
            aliases: vec![],
        });

        g.insert_node(SkillNode {
            id: "bug:autonomous_repro_and_repair".to_string(),
            name: "Autonomous Bug Reproduction and Gate-Driven Repair".to_string(),
            intent: "Reproduce a reported bug with a failing gate test first, generate candidate repair via free-tier queue, and verify test turns green".to_string(),
            preconditions: vec!["bug:report_given".to_string()],
            postconditions: vec!["test:pre_failure_proven".to_string(), "patch:applied".to_string(), "gate:all_passing".to_string()],
            tags: vec!["bugfix".to_string(), "gate".to_string(), "fitness".to_string(), "repair".to_string()],
            aliases: vec!["bug:repro_and_repair".to_string()],
        });
        let _ = g.add_edge(
            "bug:autonomous_repro_and_repair",
            SkillEdge::RequiresInterface("graph:fitness/evaluator".to_string()),
        );
        let _ = g.add_edge(
            "bug:autonomous_repro_and_repair",
            SkillEdge::RequiresInterface("holon:llmbatch/queue".to_string()),
        );
        let _ = g.add_edge(
            "bug:autonomous_repro_and_repair",
            SkillEdge::ComposesSkill("test:author_failing_gate".to_string()),
        );
        let _ = g.add_edge(
            "bug:autonomous_repro_and_repair",
            SkillEdge::ComposesSkill("gate:prove_initial_failure".to_string()),
        );
        let _ = g.add_edge(
            "bug:autonomous_repro_and_repair",
            SkillEdge::ComposesSkill("code:generate_repair".to_string()),
        );
        let _ = g.add_edge(
            "bug:autonomous_repro_and_repair",
            SkillEdge::ComposesSkill("gate:verify_green".to_string()),
        );

        // ---------------------------------------------------------------------------------
        // Skill 3: Autonomous Skill Registry Hygiene & Deduplication (skill:curate_registry)
        // ---------------------------------------------------------------------------------
        g.insert_node(SkillNode {
            id: "skill:audit_step".to_string(),
            name: "Audit Skill Registry Graph".to_string(),
            intent: "Execute graph audit to find duplicates, monoliths, and broken prerequisites"
                .to_string(),
            preconditions: vec!["graph:initialized".to_string()],
            postconditions: vec!["issues:collected".to_string()],
            tags: vec!["audit".to_string(), "hygiene".to_string()],
            aliases: vec![],
        });
        let _ =
            g.add_edge("skill:audit_step", SkillEdge::RequiresInterface("skill:audit".to_string()));
        let _ = g.add_edge(
            "skill:audit_step",
            SkillEdge::RequiresInterface("wasi:filesystem".to_string()),
        );

        g.insert_node(SkillNode {
            id: "skill:plan_curation_refactor".to_string(),
            name: "Plan Skill Refactoring".to_string(),
            intent: "Generate canonical skill schemas with aliases and decompose monolithic DAGs"
                .to_string(),
            preconditions: vec!["issues:collected".to_string()],
            postconditions: vec!["refactor:planned".to_string()],
            tags: vec!["refactor".to_string()],
            aliases: vec![],
        });
        let _ = g.add_edge(
            "skill:plan_curation_refactor",
            SkillEdge::RequiresInterface("skill:graph".to_string()),
        );

        g.insert_node(SkillNode {
            id: "skill:propose_curation_pr".to_string(),
            name: "Propose Curation Pull Request".to_string(),
            intent:
                "Open a pull request with refactored TOML skill definitions and DAG verification"
                    .to_string(),
            preconditions: vec!["refactor:planned".to_string()],
            postconditions: vec!["pr:opened".to_string()],
            tags: vec!["pr".to_string()],
            aliases: vec![],
        });
        let _ = g.add_edge(
            "skill:propose_curation_pr",
            SkillEdge::RequiresInterface("agent:propose_pr".to_string()),
        );

        g.insert_node(SkillNode {
            id: "skill:curate_registry".to_string(),
            name: "Periodic Skill Registry Hygiene & Deduplication".to_string(),
            intent: "Audit skill graph for near-duplicate skills, bloated DAGs needing decomposition, and submit refactoring PRs".to_string(),
            preconditions: vec!["skill_registry_loaded".to_string(), "git_workspace_clean".to_string()],
            postconditions: vec!["overlaps_resolved".to_string(), "pr_proposed_if_needed".to_string()],
            tags: vec!["curator".to_string(), "governance".to_string(), "refactor".to_string(), "batch".to_string()],
            aliases: vec!["skill:audit_hygiene".to_string(), "skill:dedup_and_split".to_string()],
        });
        let _ = g.add_edge(
            "skill:curate_registry",
            SkillEdge::ComposesSkill("skill:audit_step".to_string()),
        );
        let _ = g.add_edge(
            "skill:curate_registry",
            SkillEdge::ComposesSkill("skill:plan_curation_refactor".to_string()),
        );
        let _ = g.add_edge(
            "skill:curate_registry",
            SkillEdge::ComposesSkill("skill:propose_curation_pr".to_string()),
        );

        // Composed skill: Autonomous Agent Feature Delivery
        g.insert_node(SkillNode {
            id: "agent:deliver_feature".to_string(),
            name: "Autonomous Feature Delivery".to_string(),
            intent: "Generate code on free tier batch queue, compile, evaluate gate checks, and propose PR".to_string(),
            preconditions: vec!["goal:specified".to_string()],
            postconditions: vec!["feature:landed_or_pr_opened".to_string()],
            tags: vec!["agent".to_string(), "swarm".to_string(), "autonomous".to_string()],
            aliases: vec![],
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

fn tokenize_skill(node: &SkillNode) -> BTreeSet<String> {
    let mut tokens = BTreeSet::new();
    let text = format!("{} {} {}", node.name, node.intent, node.tags.join(" "));
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let w = word.trim().to_lowercase();
        if w.len() >= 3 {
            tokens.insert(w);
        }
    }
    tokens
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
    fn wit_contract_first_scaffold_plans_correct_subskills_and_interfaces() {
        let g = SkillGraph::default_skills();
        let plan = g.plan("wit:contract_first_scaffold").unwrap();
        let step_ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert!(step_ids.contains(&"wit:draft_interface"));
        assert!(step_ids.contains(&"wit:validate_ast"));
        assert!(step_ids.contains(&"bindings:generate_xtask"));
        assert!(step_ids.contains(&"component:scaffold_crate"));
        assert!(plan.required_interfaces.contains("wit:reflect@0.1.0"));
        assert!(plan.required_interfaces.contains("wasi:filesystem"));
    }

    #[test]
    fn bug_autonomous_repro_and_repair_plans_gate_flow() {
        let g = SkillGraph::default_skills();
        let plan = g.plan("bug:autonomous_repro_and_repair").unwrap();
        let step_ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert!(step_ids.contains(&"test:author_failing_gate"));
        assert!(step_ids.contains(&"gate:prove_initial_failure"));
        assert!(step_ids.contains(&"code:generate_repair"));
        assert!(step_ids.contains(&"gate:verify_green"));
        assert!(plan.required_interfaces.contains("graph:fitness/evaluator"));
        assert!(plan.required_interfaces.contains("holon:llmbatch/queue"));
    }

    #[test]
    fn curate_registry_plans_curation_steps_and_alias_lookup() {
        let g = SkillGraph::default_skills();
        let plan = g.plan("skill:curate_registry").unwrap();
        let step_ids: Vec<&str> = plan.steps.iter().map(|s| s.id.as_str()).collect();
        assert!(step_ids.contains(&"skill:audit_step"));
        assert!(step_ids.contains(&"skill:plan_curation_refactor"));
        assert!(step_ids.contains(&"skill:propose_curation_pr"));
        assert!(plan.required_interfaces.contains("skill:audit"));
        assert!(plan.required_interfaces.contains("agent:propose_pr"));

        // Alias resolution
        let alias_plan = g.plan("skill:audit_hygiene").unwrap();
        assert_eq!(alias_plan.root_skill.id, "skill:curate_registry");
    }

    #[test]
    fn audit_detects_duplicates_and_monoliths() {
        let mut g = SkillGraph::default_skills();

        // Register a near-duplicate skill
        g.insert_node(SkillNode {
            id: "git:open_pr".to_string(),
            name: "Open Git Pull Request".to_string(),
            intent: "Create git branch, commit modified files, and open a PR with graph provenance"
                .to_string(),
            preconditions: vec!["workspace:files_modified".to_string()],
            postconditions: vec!["git:pr_opened".to_string()],
            tags: vec!["git".to_string(), "forge".to_string(), "pr".to_string()],
            aliases: vec![],
        });
        let _ = g.add_edge(
            "git:open_pr",
            SkillEdge::RequiresInterface("git:forge/repo@0.1.0".to_string()),
        );

        // Register a monolithic skill
        g.insert_node(SkillNode {
            id: "monolith:all_in_one".to_string(),
            name: "Mega Monolith Pipeline".to_string(),
            intent: "Run all things at once".to_string(),
            preconditions: vec![],
            postconditions: vec![],
            tags: vec![],
            aliases: vec![],
        });
        for i in 1..=6 {
            let _ = g.add_edge(
                "monolith:all_in_one",
                SkillEdge::RequiresInterface(format!("cap:system_{i}")),
            );
        }

        let issues = g.audit();
        assert!(issues.iter().any(|iss| matches!(iss, SkillAuditIssue::Duplicate { skill_a, skill_b, .. } if (skill_a == "git:open_pr" && skill_b == "git:propose_pr") || (skill_a == "git:propose_pr" && skill_b == "git:open_pr"))));
        assert!(issues.iter().any(|iss| matches!(iss, SkillAuditIssue::Monolithic { skill_id, .. } if skill_id == "monolith:all_in_one")));
    }

    #[test]
    fn declarative_toml_parsing_and_planning() {
        let toml_str = r#"
id = "custom:deploy"
name = "Custom Deploy Skill"
intent = "Deploy component to fleet"
preconditions = ["built"]
postconditions = ["serving"]
tags = ["deploy"]
aliases = ["legacy:deploy_comp"]
requires = ["fleet:deploy@0.1.0"]
composes = []
carries = ["comp-deploy"]
"#;
        let g = SkillGraph::from_toml(toml_str).unwrap();
        let plan = g.plan("custom:deploy").unwrap();
        assert_eq!(plan.root_skill.name, "Custom Deploy Skill");
        assert!(plan.required_interfaces.contains("fleet:deploy@0.1.0"));

        let alias_node = g.get_node("legacy:deploy_comp").unwrap();
        assert_eq!(alias_node.id, "custom:deploy");
    }
}
