//! Keep the photo-critic app serving. The test fleet tears down when its handle
//! drops; this holds the handle and blocks, so the app stays up for as long as
//! this process runs. Prints the ingress port for a front proxy / tailscale.
use std::time::Duration;
use comp_reconciler::fleet::{repo_root, Fleet};

fn main() {
    let home = std::env::var("HOME").unwrap();
    let key = format!("{home}/.comp-secrets/anthropic");
    let dir = repo_root().join("components/target/wasm32-wasip2/release");
    let art = vec![format!("gate={}", dir.join("photo_critic.wasm").display())];
    let fixture = repo_root().join("fixtures/photo-critic.yaml");
    let fleet = Fleet::start_with_secrets(
        "photo",
        &[fixture.to_str().unwrap()],
        &art,
        &[format!("vault://acme/anthropic=@{key}")],
    );
    println!("PHOTO_INGRESS_PORT={}", fleet.ingress_port);
    println!("host header: photo.acme.test");
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
