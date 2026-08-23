//! `budget-domain` — record spending against per-category budgets and report the balance

#[allow(warnings)]
mod bindings;

use bindings::Guest;
use bindings::wasi::keyvalue::store::open;
use bindings::auth::identity::authorizer::introspect;
use serde::{Serialize, Deserialize};
use std::collections::HashMap;

#[derive(Serialize, Deserialize, Default)]
struct BudgetState {
    balance: f64,
    categories: HashMap<String, f64>,
}

struct Component;

impl Component {
    fn get_state(principal: &str) -> BudgetState {
        let bucket = open("default").unwrap();
        let key = format!("budget_{}", principal);
        match bucket.get(&key) {
            Ok(Some(bytes)) => serde_json::from_slice(&bytes).unwrap_or_default(),
            _ => BudgetState::default(),
        }
    }

    fn save_state(principal: &str, state: &BudgetState) {
        let bucket = open("default").unwrap();
        let key = format!("budget_{}", principal);
        let bytes = serde_json::to_vec(state).unwrap();
        bucket.set(&key, &bytes).unwrap();
    }
    
    fn validate_token(token: &str) -> String {
        // Just use introspect to get the principal.subject
        match introspect(token) {
            Ok(principal) => principal.subject,
            Err(_) => panic!("Unauthorized"),
        }
    }
}

impl Guest for Component {
    fn add_transaction(token: String, amount: f64, category: String, is_income: bool) {
        let principal = Self::validate_token(&token);
        let mut state = Self::get_state(&principal);
        if is_income {
            state.balance += amount;
        } else {
            state.balance -= amount;
            *state.categories.entry(category).or_insert(0.0) -= amount;
        }
        Self::save_state(&principal, &state);
    }

    fn get_balance(token: String) -> f64 {
        let principal = Self::validate_token(&token);
        let state = Self::get_state(&principal);
        state.balance
    }

    fn get_category_budget(token: String, category: String) -> f64 {
        let principal = Self::validate_token(&token);
        let state = Self::get_state(&principal);
        *state.categories.get(&category).unwrap_or(&0.0)
    }
}

bindings::export!(Component with_types_in bindings);
