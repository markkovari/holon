use once_cell::sync::Lazy;
use std::sync::Mutex;
use std::collections::HashMap;

struct BudgetState {
    balance: f64,
    categories: HashMap<String, f64>,
}

static STATE: Lazy<Mutex<BudgetState>> = Lazy::new(|| {
    Mutex::new(BudgetState {
        balance: 0.0,
        categories: HashMap::new(),
    })
});

pub fn add_transaction(amount: f64, category: String, is_income: bool) {
    let mut state = STATE.lock().unwrap();
    if is_income {
        state.balance += amount;
    } else {
        state.balance -= amount;
        *state.categories.entry(category).or_insert(0.0) -= amount;
    }
}

pub fn get_balance() -> f64 {
    STATE.lock().unwrap().balance
}

pub fn get_category_budget(category: String) -> f64 {
    *STATE.lock().unwrap().categories.get(&category).unwrap_or(&0.0)
}
