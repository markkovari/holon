//! Keep the contrast-audit app serving. The test fleet tears down when its handle
//! drops; this holds the handle and blocks, so the app stays up for as long as
//! this process runs. Prints the ingress port for a front proxy / tailscale.
use comp_reconciler::fleet::serve_fixture;

fn main() {
    serve_fixture("contrast", "contrast_audit.wasm", "contrast-audit");
}
