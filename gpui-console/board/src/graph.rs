use std::collections::{BTreeMap, BTreeSet};

use super::map::{Key, Kind, Map, Proof};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rel {
    Uses,
    Carries,
    Added,
    Wrote,
    Tried,
    Touched,
    Ran,
    Composes,
    Requires,
    EquippedWith,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Above,
    Left,
    Right,
    Below,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Node {
    pub key: Key,
    pub label: String,
    pub sub: String,
    pub kind: Kind,
    pub weight: u32,
    pub proof: Vec<Proof>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Lane {
    pub title: &'static str,
    pub side: Side,
    pub keys: Vec<Key>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Graph {
    pub nodes: BTreeMap<Key, Node>,
    pub rels: BTreeSet<(Key, Rel, Key)>,
}

pub struct SessionNode {
    pub root: String,
    pub lead: String,
    pub title: String,
    pub sub: String,
    pub touched: BTreeSet<String>,
}

impl Graph {
    pub fn build(
        components: &Map,
        knowledge: &Map,
        sessions: &[SessionNode],
        agents: &[super::agents::AgentInfo],
    ) -> Graph {
        let mut g = Graph::default();
        for a in agents {
            let key = format!("agent:{}", a.name);
            let sub =
                if a.description.is_empty() { a.model.clone() } else { a.description.clone() };
            g.nodes.insert(
                key.clone(),
                Node {
                    key,
                    label: a.name.clone(),
                    sub,
                    kind: Kind::Agent,
                    weight: 0,
                    proof: Vec::new(),
                },
            );
        }
        for (_, i) in components.items().chain(knowledge.items()) {
            g.nodes.entry(i.key.clone()).or_insert_with(|| Node {
                key: i.key.clone(),
                label: i.label.clone(),
                sub: i.sub.clone(),
                kind: i.kind,
                weight: i.weight,
                proof: i.proof.clone(),
            });
        }
        for (a, b, why) in &components.edges {
            g.rels.insert((
                a.clone(),
                if why == "composes" { Rel::Carries } else { Rel::Uses },
                b.clone(),
            ));
        }
        for f in &knowledge.frames {
            let proof = f
                .items
                .iter()
                .flat_map(|i| &i.proof)
                .filter(|p| matches!(p, Proof::Link { .. }))
                .cloned()
                .collect();
            g.nodes.insert(
                f.key.clone(),
                Node {
                    key: f.key.clone(),
                    label: f.title.clone(),
                    sub: f.sub.clone(),
                    kind: Kind::Run,
                    weight: 0,
                    proof,
                },
            );
            for i in &f.items {
                let rel = if i.key.starts_with("component:") { Rel::Added } else { Rel::Tried };
                g.rels.insert((f.key.clone(), rel, i.key.clone()));
            }
        }
        for (a, b, _) in &knowledge.edges {
            g.rels.insert((a.clone(), Rel::Wrote, b.clone()));
        }
        for s in sessions {
            let key = format!("session:{}", s.root);
            g.nodes.insert(
                key.clone(),
                Node {
                    key: key.clone(),
                    label: s.title.clone(),
                    sub: s.sub.clone(),
                    kind: Kind::Session,
                    weight: 0,
                    proof: Vec::new(),
                },
            );
            let agent = format!("agent:{}", s.lead);
            if g.nodes.contains_key(&agent) {
                g.rels.insert((agent, Rel::Ran, key.clone()));
            }
            for c in &s.touched {
                let to = super::map::component_key(c);
                if g.nodes.contains_key(&to) {
                    g.rels.insert((key.clone(), Rel::Touched, to));
                }
            }
        }
        g
    }

    pub fn with_skills(
        mut self,
        skill_graph: &holon_skills::SkillGraph,
        agents: &[super::agents::AgentInfo],
    ) -> Self {
        for (id, skill) in &skill_graph.nodes {
            let key = format!("skill:{id}");
            let mut proof = Vec::new();
            for pre in &skill.preconditions {
                proof.push(Proof::Check {
                    id: "precondition".to_string(),
                    passed: true,
                    detail: pre.clone(),
                });
            }
            for post in &skill.postconditions {
                proof.push(Proof::Check {
                    id: "postcondition".to_string(),
                    passed: true,
                    detail: post.clone(),
                });
            }
            self.nodes.insert(
                key.clone(),
                Node {
                    key: key.clone(),
                    label: skill.name.clone(),
                    sub: skill.intent.clone(),
                    kind: Kind::Skill,
                    weight: 1,
                    proof,
                },
            );

            if let Some(edges) = skill_graph.edges.get(id) {
                for edge in edges {
                    match edge {
                        holon_skills::SkillEdge::ComposesSkill(sub) => {
                            let to = format!("skill:{sub}");
                            self.rels.insert((key.clone(), Rel::Composes, to));
                        }
                        holon_skills::SkillEdge::RequiresInterface(iface) => {
                            let to = format!("component:{iface}");
                            self.nodes.entry(to.clone()).or_insert_with(|| Node {
                                key: to.clone(),
                                label: iface.clone(),
                                sub: "WIT capability".to_string(),
                                kind: Kind::Part,
                                weight: 0,
                                proof: Vec::new(),
                            });
                            self.rels.insert((key.clone(), Rel::Requires, to));
                        }
                        _ => {}
                    }
                }
            }

            for alias in &skill.aliases {
                let alias_key = format!("skill:{alias}");
                self.nodes.insert(
                    alias_key.clone(),
                    Node {
                        key: alias_key.clone(),
                        label: format!("alias: {alias}"),
                        sub: format!("routes to {id}"),
                        kind: Kind::Skill,
                        weight: 0,
                        proof: Vec::new(),
                    },
                );
                self.rels.insert((alias_key, Rel::Composes, key.clone()));
            }
        }

        // Link agents to equipped skills
        for a in agents {
            let agent_key = format!("agent:{}", a.name);
            for c in &a.capabilities {
                let skill_key = format!("skill:{c}");
                if self.nodes.contains_key(&skill_key) {
                    self.rels.insert((agent_key.clone(), Rel::EquippedWith, skill_key));
                }
            }
        }

        self
    }

    pub fn lanes(&self, key: &str) -> Vec<Lane> {
        const LANES: [(Rel, bool, &str, Side); 19] = [
            (Rel::Ran, false, "run by", Side::Above),
            (Rel::Added, false, "added by", Side::Above),
            (Rel::Wrote, false, "written by", Side::Above),
            (Rel::Tried, false, "attempt of", Side::Above),
            (Rel::EquippedWith, false, "equipped by", Side::Above),
            (Rel::Uses, true, "uses", Side::Left),
            (Rel::Carries, true, "made of", Side::Left),
            (Rel::Composes, false, "sub-skill of", Side::Left),
            (Rel::Uses, false, "used by", Side::Right),
            (Rel::Carries, false, "part of", Side::Right),
            (Rel::Composes, true, "composes", Side::Right),
            (Rel::Tried, true, "attempts", Side::Below),
            (Rel::Added, true, "added", Side::Below),
            (Rel::Wrote, true, "wrote", Side::Below),
            (Rel::Touched, false, "touched in", Side::Below),
            (Rel::Touched, true, "touched", Side::Below),
            (Rel::Requires, true, "requires", Side::Below),
            (Rel::EquippedWith, true, "equipped with", Side::Below),
            (Rel::Ran, true, "ran", Side::Below),
        ];
        LANES
            .iter()
            .filter_map(|&(rel, out, title, side)| {
                let mut keys: Vec<Key> = self
                    .rels
                    .iter()
                    .filter(|(a, r, b)| *r == rel && if out { a == key } else { b == key })
                    .map(|(a, _, b)| if out { b.clone() } else { a.clone() })
                    .filter(|k| k != key)
                    .collect();
                keys.dedup();
                keys.sort_by(|a, b| {
                    let w = |k: &Key| self.nodes.get(k).map_or(0, |n| n.weight);
                    w(b).cmp(&w(a)).then_with(|| self.label(a).cmp(&self.label(b)))
                });
                (!keys.is_empty()).then_some(Lane { title, side, keys })
            })
            .collect()
    }

    pub fn degree(&self, key: &str) -> usize {
        self.rels.iter().filter(|(a, _, b)| a == key || b == key).count()
    }

    pub fn label(&self, key: &str) -> String {
        self.nodes.get(key).map_or_else(
            || key.split_once(':').map_or(key, |(_, n)| n).to_string(),
            |n| n.label.clone(),
        )
    }

    pub fn search(&self, query: &str, limit: usize) -> Vec<Key> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        let mut hits: Vec<(u8, u32, &Node)> = self
            .nodes
            .values()
            .filter_map(|n| {
                let (l, s) = (n.label.to_lowercase(), n.sub.to_lowercase());
                let rank = if l.starts_with(&q) {
                    0
                } else if l.contains(&q) {
                    1
                } else if s.contains(&q) {
                    2
                } else {
                    return None;
                };
                Some((rank, n.weight, n))
            })
            .collect();
        hits.sort_by(|a, b| {
            a.0.cmp(&b.0).then(b.1.cmp(&a.1)).then_with(|| a.2.label.cmp(&b.2.label))
        });
        hits.into_iter().take(limit).map(|(_, _, n)| n.key.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::{component_key, Frame, Item};

    fn item(key: &str, label: &str, kind: Kind, weight: u32) -> Item {
        Item {
            key: key.into(),
            label: label.into(),
            sub: String::new(),
            kind,
            weight,
            proof: vec![],
        }
    }

    fn graph() -> Graph {
        let c = |n: &str| component_key(n);
        let components = Map {
            frames: vec![Frame {
                key: "f".into(),
                title: String::new(),
                sub: String::new(),
                items: vec![
                    item(&c("shop"), "shop", Kind::App, 2),
                    item(&c("store"), "record-store", Kind::Part, 3),
                    item(&c("paginate"), "paginate", Kind::Part, 1),
                ],
            }],
            edges: vec![
                (c("shop"), c("store"), "records:store/store".into()),
                (c("paginate"), c("store"), "records:store/store".into()),
                (c("shop"), c("paginate"), "composes".into()),
            ],
        };
        let knowledge = Map {
            frames: vec![Frame {
                key: "run:7".into(),
                title: "add pagination".into(),
                sub: "merged".into(),
                items: vec![
                    item("attempt:7/mvp", "round 1 · mvp", Kind::Attempt, 1000),
                    item(&c("paginate"), "paginate", Kind::Part, 0),
                ],
            }],
            edges: vec![("attempt:7/mvp".into(), c("paginate"), "wrote".into())],
        };
        let sessions = [SessionNode {
            root: "r1".into(),
            lead: "lead".into(),
            title: "rate-limit the shop".into(),
            sub: String::new(),
            touched: ["paginate".to_string(), "not-a-component".to_string()].into(),
        }];
        let agents = [crate::agents::AgentInfo {
            name: "lead".into(),
            description: "plans".into(),
            ..Default::default()
        }];
        Graph::build(&components, &knowledge, &sessions, &agents)
    }

    #[test]
    fn a_component_is_seen_from_where_it_came_from_what_it_uses_and_what_uses_it() {
        let g = graph();
        let lanes = g.lanes(&component_key("paginate"));
        let got: Vec<(&str, Side, Vec<String>)> = lanes
            .iter()
            .map(|l| (l.title, l.side, l.keys.iter().map(|k| g.label(k)).collect()))
            .collect();
        assert_eq!(
            got,
            [
                ("added by", Side::Above, vec!["add pagination".to_string()]),
                ("written by", Side::Above, vec!["round 1 · mvp".to_string()]),
                ("uses", Side::Left, vec!["record-store".to_string()]),
                ("part of", Side::Right, vec!["shop".to_string()]),
                ("touched in", Side::Below, vec!["rate-limit the shop".to_string()]),
            ]
        );
        assert_eq!(g.nodes[&component_key("paginate")].weight, 1);
    }

    #[test]
    fn a_run_is_seen_with_its_attempts_and_what_it_added_and_a_session_with_what_it_touched() {
        let g = graph();
        let run: Vec<&str> = g.lanes("run:7").iter().map(|l| l.title).collect();
        assert_eq!(run, ["attempts", "added"]);
        let s = g.lanes("session:r1");
        assert_eq!(
            s.iter().map(|l| (l.title, l.keys.len())).collect::<Vec<_>>(),
            [("run by", 1), ("touched", 1)],
            "only what is a component"
        );
        assert_eq!(
            g.lanes("agent:lead")[0].title,
            "ran",
            "and the agent knows the sessions it was given"
        );
        assert_eq!(g.degree(&component_key("store")), 2);
    }

    #[test]
    fn a_search_finds_names_first_then_what_mentions_it() {
        let g = graph();
        assert_eq!(g.search("pag", 5)[0], component_key("paginate"));
        assert_eq!(g.search("store", 5), [component_key("store")], "record-store holds it");
        assert!(g.search("  ", 5).is_empty());
        let all = g.search("a", 50);
        assert_eq!((all.len(), all.iter().collect::<BTreeSet<_>>().len()), (4, 4));
    }

    #[test]
    fn skills_are_seen_with_composed_subskills_and_required_interfaces() {
        let g = graph();
        let skill_graph = holon_skills::SkillGraph::default_skills();
        let agents = [crate::agents::AgentInfo {
            name: "scaffolder".into(),
            capabilities: vec!["wit:contract_first_scaffold".into()],
            ..Default::default()
        }];
        let g = g.with_skills(&skill_graph, &agents);

        assert!(g.nodes.contains_key("skill:wit:contract_first_scaffold"));
        assert_eq!(g.nodes["skill:wit:contract_first_scaffold"].kind, Kind::Skill);

        let lanes = g.lanes("skill:wit:contract_first_scaffold");
        let titles: Vec<&str> = lanes.iter().map(|l| l.title).collect();
        assert!(titles.contains(&"composes"));
        assert!(titles.contains(&"requires"));
        assert!(titles.contains(&"equipped by"));

        let composes_lane = lanes.iter().find(|l| l.title == "composes").unwrap();
        assert!(composes_lane.keys.contains(&"skill:wit:draft_interface".to_string()));
        assert!(composes_lane.keys.contains(&"skill:bindings:generate_xtask".to_string()));
    }
}
