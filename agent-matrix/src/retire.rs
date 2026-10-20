//! Absence may also mean a runtime on the wrong state dir, so a deletion is believed only after
//! `PASSES` misses and never for most of the fleet at once (ADR-0105 §3).

use std::collections::{HashMap, HashSet};

pub const PASSES: u32 = 3;

#[derive(Debug, PartialEq)]
pub enum Verdict {
    Nothing,
    Retire(Vec<String>),
    Refuse(Vec<String>),
}

#[derive(Default)]
pub struct Absence(HashMap<String, u32>);

impl Absence {
    pub fn observe(&mut self, known: &[String], active: &HashSet<String>) -> Verdict {
        self.0.retain(|name, _| known.contains(name));
        for name in known {
            if active.contains(name) {
                self.0.remove(name);
            } else {
                *self.0.entry(name.clone()).or_default() += 1;
            }
        }
        let mut gone: Vec<String> = known
            .iter()
            .filter(|n| self.0.get(*n).copied().unwrap_or(0) >= PASSES)
            .cloned()
            .collect();
        gone.sort();
        if gone.is_empty() {
            return Verdict::Nothing;
        }
        // Over half the fleet at once is far likelier a runtime that lost its state than deletions.
        if gone.len() > 1 && gone.len() * 2 > known.len() {
            return Verdict::Refuse(gone);
        }
        for name in &gone {
            self.0.remove(name);
        }
        Verdict::Retire(gone)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn set(v: &[&str]) -> HashSet<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_missing_agent_is_retired_after_three_passes_and_not_before() {
        let mut a = Absence::default();
        let known = names(&["coach", "rower", "kevin"]);
        let active = set(&["coach", "rower"]);
        assert_eq!(a.observe(&known, &active), Verdict::Nothing);
        assert_eq!(a.observe(&known, &active), Verdict::Nothing);
        assert_eq!(a.observe(&known, &active), Verdict::Retire(names(&["kevin"])));
    }

    #[test]
    fn coming_back_resets_the_count() {
        let mut a = Absence::default();
        let known = names(&["coach", "kevin"]);
        a.observe(&known, &set(&["coach"]));
        a.observe(&known, &set(&["coach"]));
        assert_eq!(a.observe(&known, &set(&["coach", "kevin"])), Verdict::Nothing);
        a.observe(&known, &set(&["coach"]));
        assert_eq!(a.observe(&known, &set(&["coach"])), Verdict::Nothing, "two passes again");
    }

    #[test]
    fn most_of_the_fleet_vanishing_at_once_is_refused() {
        let mut a = Absence::default();
        let known = names(&["a", "b", "c", "d"]);
        let active = set(&["d"]);
        a.observe(&known, &active);
        a.observe(&known, &active);
        assert_eq!(a.observe(&known, &active), Verdict::Refuse(names(&["a", "b", "c"])));
        assert_eq!(
            a.observe(&known, &active),
            Verdict::Refuse(names(&["a", "b", "c"])),
            "it keeps refusing until the runtime lists them again"
        );
        assert_eq!(a.observe(&known, &set(&["a", "b", "c", "d"])), Verdict::Nothing);
    }

    #[test]
    fn two_of_four_is_a_deletion_and_two_of_three_is_not() {
        let mut a = Absence::default();
        let four = names(&["a", "b", "c", "d"]);
        for _ in 0..2 {
            a.observe(&four, &set(&["c", "d"]));
        }
        assert_eq!(a.observe(&four, &set(&["c", "d"])), Verdict::Retire(names(&["a", "b"])));
        let mut b = Absence::default();
        let three = names(&["a", "b", "c"]);
        for _ in 0..2 {
            b.observe(&three, &set(&["c"]));
        }
        assert_eq!(b.observe(&three, &set(&["c"])), Verdict::Refuse(names(&["a", "b"])));
    }

    #[test]
    fn a_lone_agent_deleted_is_retired() {
        let mut a = Absence::default();
        let known = names(&["only"]);
        a.observe(&known, &set(&[]));
        a.observe(&known, &set(&[]));
        assert_eq!(a.observe(&known, &set(&[])), Verdict::Retire(names(&["only"])));
    }
}
