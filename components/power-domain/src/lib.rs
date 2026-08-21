use bindings::wasi::keyvalue::store::open;
use bindings::auth::identity::authorizer::authorize;
use bindings::auth::identity::types::Permission;

mod bindings;

struct PowerDomain;

impl bindings::Guest for PowerDomain {
    fn calculate_cost(wattage: f32, hours: f32, token: String) -> Result<f32, String> {
        // Authenticate the user
        let perm = Permission {
            target: "power".to_string(),
            action: "calculate".to_string(),
        };
        
        let _principal = authorize(&token, &perm).map_err(|_| "Auth failed".to_string())?;

        // Replace in-memory state with wasi:keyvalue persistence
        let bucket = open("default").map_err(|_| "Failed to open bucket".to_string())?;
        
        // Get price, fallback to 0.15
        let price = match bucket.get("price").map_err(|_| "Failed to get price".to_string())? {
            Some(price_bytes) => {
                let price_str = String::from_utf8(price_bytes).map_err(|e| e.to_string())?;
                price_str.parse::<f32>().unwrap_or(0.15)
            },
            None => {
                let default_price = 0.15_f32;
                let _ = bucket.set("price", default_price.to_string().as_bytes());
                default_price
            }
        };

        let kwh = (wattage * hours) / 1000.0;
        Ok(kwh * price)
    }
}

bindings::export!(PowerDomain with_types_in bindings);
