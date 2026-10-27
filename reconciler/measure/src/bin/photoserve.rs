//! Keep the photo-critic app serving. The test fleet tears down when its handle
//! drops; this holds the handle and blocks, so the app stays up for as long as
//! this process runs. Prints the ingress port for a front proxy / tailscale.
use holon_fleet::fleet::serve_fixture;

fn main() {
    serve_fixture("photo", "photo_critic.wasm", "photo-critic");
}
