use comp_reconciler::fleet::{repo_root, Fleet};
use serde_json::json;

#[test]
fn graphql_gateway_ping() {
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    
    // Compose graphql-gateway with proxy-route to satisfy the proxy:route import
    let catalog = comp_reconciler::plug::Catalog::scan(&[dir.clone()]);
    let composed = comp_reconciler::plug::compose("graphql-gateway", &catalog)
        .expect("graphql-gateway composes with proxy-route");
        
    let composed_path = std::env::temp_dir().join("graphql_gateway_composed.wasm");
    std::fs::write(&composed_path, composed).unwrap();

    let artifacts = vec![format!("graphql={}", composed_path.display())];

    let yaml = r#"
version: comp/v1
app: GraphQLGateway
tenant: acme
strategy: linked
components:
  - id: graphql
    host_needs:
      - wasi:http/incoming-handler
      - wasi:http/outgoing-handler
ingress:
  host: graphql.test
  component: graphql
"#;
    let spec = std::env::temp_dir().join("graphql-fleet.yaml");
    std::fs::write(&spec, yaml).unwrap();

    let fleet = Fleet::start_with_secrets("graphql", &[spec.to_str().unwrap()], &artifacts, &[]);

    let client = reqwest::blocking::Client::new();
    
    // Test the ping query
    let body = json!({
        "query": "{ ping }"
    });
    
    let mut out = serde_json::Value::Null;
    
    fleet.until("gateway responds to ping", std::time::Duration::from_secs(30), || {
        let resp = match client
            .post(format!("http://127.0.0.1:{}/graphql", fleet.ingress_port))
            .header("host", "graphql.test")
            .json(&body)
            .send()
        {
            Ok(r) => r,
            Err(e) => return Err(e.to_string()),
        };
        
        if resp.status() == 200 {
            out = resp.json().expect("parse json");
            Ok(())
        } else {
            Err(format!("HTTP {}", resp.status()))
        }
    });
        
    assert_eq!(out["data"]["ping"], "pong");
}
