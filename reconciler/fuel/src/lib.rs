//! What a run costs and the spend a project budget is checked against: the price table, the
//! off-peak window one vendor discounts inside, and a run's total spend (goal 01, "fuel is money";
//! docs/adr/0081).
//!
//! Pure functions over plain values, with no dependency beyond `std`: the prices are the only thing
//! here that will ever be wrong, and a wrong price is a one-line fix with a test beside it.

/// What a whole run has spent, from the attempts' priced usage.
pub mod budget;
/// What a run costs, in cents and in micro-USD.
pub mod cost;
/// Is DeepSeek's off-peak discount active right now?
pub mod offpeak;
