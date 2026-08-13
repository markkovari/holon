// The tag is read at compile time, so cargo has to be told that changing it
// invalidates the build. Without this the second build is a cache hit, the wasm
// is byte-identical, and a test that thinks it shipped a new version has shipped
// nothing — while passing.
fn main() {
    println!("cargo:rerun-if-env-changed=COMP_VERSION_TAG");
}
