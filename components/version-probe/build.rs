// The tag is read at compile time: without this a changed tag is a cache hit, and a test
// "ships" a byte-identical wasm while passing.
fn main() {
    println!("cargo:rerun-if-env-changed=COMP_VERSION_TAG");
}
