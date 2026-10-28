//! Food recognition and calorie estimation domain component (ADR-0125).

use serde::{Deserialize, Serialize};

pub const VARIETIES: &[&str] = &[
    "salad",
    "soup or stew",
    "pasta",
    "rice or grain dish",
    "meat dish",
    "fish or seafood dish",
    "vegetable dish",
    "sandwich or burger",
    "pizza",
    "eggs or breakfast",
    "bread or pastry",
    "fruit",
    "dessert or sweets",
    "snack",
    "drink",
    "other",
];

/// Jev's score is the probability-weighted band number, so a fraction lands between two bands.
pub const BANDS: &[(&str, f64)] = &[
    ("a drink or a bite, under 150 kcal", 75.0),
    ("a small portion, 150 to 300 kcal", 225.0),
    ("a light meal, 300 to 450 kcal", 375.0),
    ("an ordinary meal, 450 to 600 kcal", 525.0),
    ("a large meal, 600 to 800 kcal", 700.0),
    ("a very large meal, 800 to 1000 kcal", 900.0),
    ("a huge plate, 1000 to 1300 kcal", 1150.0),
    ("an enormous plate, over 1300 kcal", 1500.0),
];

pub const NONE: &str = "none of these";
pub const MAX_CANDIDATES: usize = 12;
pub const FOOD_AT: f64 = 0.5;
pub const MATCH_AT: f64 = 0.5;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Item {
    pub name: String,
    pub variety: String,
    pub kcal: u32,
    pub seen: u32,
    pub estimated: bool,
    pub photo: Option<String>,
    pub first: u64,
    pub last: u64,
}

pub fn kcal_at(score: f64) -> u32 {
    let top = (BANDS.len() - 1) as f64;
    let s = score.clamp(0.0, top);
    let (lo, hi) = (s.floor() as usize, (s.ceil() as usize).min(BANDS.len() - 1));
    let f = s - s.floor();
    let kcal = BANDS[lo].1 * (1.0 - f) + BANDS[hi].1 * f;
    ((kcal / 10.0).round() as u32) * 10
}

pub fn percent(p: f64) -> u32 {
    (p * 100.0).round().clamp(0.0, 100.0) as u32
}

#[derive(Debug, Clone, PartialEq)]
pub struct Recognition {
    pub is_food: bool,
    pub confidence: f64,
    pub variety: String,
    pub kcal: u32,
    pub matched_item: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kcal_interpolation() {
        assert_eq!(kcal_at(0.0), 80); // 75 rounded to 80
        assert_eq!(kcal_at(1.0), 230); // 225 rounded to 230
        assert_eq!(kcal_at(3.0), 530); // 525 rounded to 530
        assert_eq!(kcal_at(7.0), 1500);
    }

    #[test]
    fn percent_clamping() {
        assert_eq!(percent(0.0), 0);
        assert_eq!(percent(0.854), 85);
        assert_eq!(percent(1.0), 100);
        assert_eq!(percent(1.5), 100);
    }
}
