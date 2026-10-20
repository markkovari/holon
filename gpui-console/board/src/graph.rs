//! Everything the board knows, as one graph: the components and what they use, the applications and
//! what they are made of, the goal runs with their attempts and what they added, and the sessions with
//! what they touched. What the Explore tab walks, one subject at a time.
//!
//! A subject's neighbours are sorted into lanes by how they are joined and drawn where the question they
//! answer is asked: above it where it came from, left what it builds on, right what builds on it, below
//! what it led to and where it appeared. Pure: maps and sessions in, nodes and lanes out.

use std::collections::{BTreeMap, BTreeSet};

use super::map::{Key, Kind, Map, Proof};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Rel {
    /// A component imports what another exports.
    Uses,
    /// An application is composed of a part.
    Carries,
    /// A goal run left the pool able to do something: it added a component.
    Added,
    /// An attempt wrote a component's files.
    Wrote,
    /// A goal run tried this attempt.
    Tried,
    /// A session's agents named or touched a component.
    Touched,
    /// An agent was given a session's goal.
    Ran,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    /// Where it came from.
    Above,
    /// What it builds on.
    Left,
    /// What builds on it.
    Right,
    /// What it led to, and where it appeared.
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

/// One group of a subject's neighbours, joined to it the same way.
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

/// A session, as the graph needs it: its root, its goal, how it stands, and the components it named.
pub struct SessionNode {
    pub root: String,
    /// The agent that was given the goal.
    pub lead: String,
    pub title: String,
    pub sub: String,
    pub touched: BTreeSet<String>,
}

impl Graph {
    /// The components, the goal runs and the sessions, joined.
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
            // a run is proved by what its winner became
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

    /// `key`'s neighbours, lane by lane, each lane heaviest first. Lanes with nobody in them are left out.
    pub fn lanes(&self, key: &str) -> Vec<Lane> {
        // (relation, whether the subject is its source, the lane's name, where it is drawn)
        const LANES: [(Rel, bool, &str, Side); 14] = [
            (Rel::Ran, false, "run by", Side::Above),
            (Rel::Added, false, "added by", Side::Above),
            (Rel::Wrote, false, "written by", Side::Above),
            (Rel::Tried, false, "attempt of", Side::Above),
            (Rel::Uses, true, "uses", Side::Left),
            (Rel::Carries, true, "made of", Side::Left),
            (Rel::Uses, false, "used by", Side::Right),
            (Rel::Carries, false, "part of", Side::Right),
            (Rel::Tried, true, "attempts", Side::Below),
            (Rel::Added, true, "added", Side::Below),
            (Rel::Wrote, true, "wrote", Side::Below),
            (Rel::Touched, false, "touched in", Side::Below),
            (Rel::Touched, true, "touched", Side::Below),
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

    /// How many neighbours `key` has, all lanes together: what a neighbour's chip says is behind it.
    pub fn degree(&self, key: &str) -> usize {
        self.rels.iter().filter(|(a, _, b)| a == key || b == key).count()
    }

    pub fn label(&self, key: &str) -> String {
        self.nodes.get(key).map_or_else(
            || key.split_once(':').map_or(key, |(_, n)| n).to_string(),
            |n| n.label.clone(),
        )
    }

    /// What a search finds: names that start with the query first, then names that hold it, then what
    /// says it in its description; each heaviest first.
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
        // the label it was first given, not the run's copy of it
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
        // paginate, add pagination, rate-limit the shop, and the agent lead: each once
        let all = g.search("a", 50);
        assert_eq!((all.len(), all.iter().collect::<BTreeSet<_>>().len()), (4, 4));
    }
}
