//! What a run costs and what it may spend: the price table, the off-peak window one vendor discounts
//! inside, and the spend, the cap and the rate limit a project budget is checked against (goal 01,
//! "fuel is money"; docs/adr/0081).
//!
//! Pure functions over plain values, with no dependency beyond `std`: the prices are the only thing
//! here that will ever be wrong, and a wrong price is a one-line fix with a test beside it.
//! `comp_reconciler::{cost, offpeak, ...}` are these modules, re-exported, so nothing that named
//! them there changed.

pub mod bucket;
/// What a whole run has spent, from the attempts' priced usage.
pub mod budget;
/// What a run costs, in cents and in micro-USD.
pub mod cost;
pub mod money;
/// Is DeepSeek's off-peak discount active right now?
pub mod offpeak;
pub mod router;
pub mod wallet;
