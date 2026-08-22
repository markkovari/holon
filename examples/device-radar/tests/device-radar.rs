use reqwest::blocking::Client;
use serde_json::json;

const URL: &str = "http://localhost:3055";

#[test]
fn test_device_radar_e2e() {
    let client = Client::new();
    
    // Register
    let res = client.post(&format!("{}/api/register", URL))
        .json(&json!({ "email": "test@example.com", "password": "password123" }))
        .send().unwrap();
    assert!(res.status().is_success() || res.status().as_u16() == 409); // 409 if already exists

    // Login
    let res = client.post(&format!("{}/api/login", URL))
        .json(&json!({ "email": "test@example.com", "password": "password123" }))
        .send().unwrap();
    assert!(res.status().is_success());
    let data: serde_json::Value = res.json().unwrap();
    let token = data["access_token"].as_str().unwrap();

    // Scan devices
    let res = client.get(&format!("{}/api/devices", URL))
        .header("Authorization", format!("Bearer {}", token))
        .send().unwrap();
    assert!(res.status().is_success());
}
