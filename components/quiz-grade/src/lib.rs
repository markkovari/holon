//! `quiz-grade` — reference implementation of `quiz:grade/grader`.
//!
//! Grade one submission against an answer key (integer-percent score + pass/fail),
//! and roll a cohort's percentage scores into gradebook stats (mean, median,
//! spread, pass count, and a 5-bin distribution). Pure compute — no state, no
//! host imports, integer math throughout.

#[allow(warnings)]
mod bindings;

use bindings::exports::quiz::grade::grader::{Guest, GradeResult, Stats};

struct Component;

impl Guest for Component {
    fn grade(answers: Vec<u32>, key: Vec<u32>, pass_mark: u32) -> GradeResult {
        let total = key.len() as u32;
        // compare position by position; a missing answer counts wrong.
        let correct = key.iter().enumerate().filter(|(i, k)| answers.get(*i) == Some(*k)).count() as u32;
        let score_pct = if total == 0 { 0 } else { (correct * 100 + total / 2) / total };
        GradeResult { correct, total, score_pct, passed: score_pct >= pass_mark }
    }

    fn distribution(scores: Vec<u32>, pass_mark: u32) -> Stats {
        let count = scores.len() as u32;
        if count == 0 {
            return Stats { count: 0, mean: 0, median: 0, min: 0, max: 0, pass_count: 0, buckets: vec![0; 5] };
        }
        let sum: u32 = scores.iter().sum();
        let mean = (sum + count / 2) / count;
        let min = *scores.iter().min().unwrap();
        let max = *scores.iter().max().unwrap();
        let pass_count = scores.iter().filter(|&&s| s >= pass_mark).count() as u32;

        let mut sorted = scores.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        let median = if n % 2 == 1 { sorted[n / 2] } else { (sorted[n / 2 - 1] + sorted[n / 2] + 1) / 2 };

        // 5 bins: [0-19, 20-39, 40-59, 60-79, 80-100].
        let mut buckets = vec![0u32; 5];
        for &s in &scores {
            let b = (s.min(100) / 20).min(4) as usize;
            buckets[b] += 1;
        }
        Stats { count, mean, median, min, max, pass_count, buckets }
    }
}

bindings::export!(Component with_types_in bindings);
