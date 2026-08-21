use std::sync::Mutex;
use once_cell::sync::Lazy;

mod bindings;

static CACHED_PRICE: Lazy<Mutex<f32>> = Lazy::new(|| Mutex::new(0.15));

struct PowerDomain;

impl bindings::Guest for PowerDomain {
    fn calculate_cost(wattage: f32, hours: f32) -> f32 {
        // Here we would use wasi:http/outgoing-handler to fetch live prices.
        // Gracefully returning the mocked price as a fallback if the network fails.
        let price = *CACHED_PRICE.lock().unwrap();
        let kwh = (wattage * hours) / 1000.0;
        kwh * price
    }
}

bindings::export!(PowerDomain with_types_in bindings);
