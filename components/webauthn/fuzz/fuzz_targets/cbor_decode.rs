#![no_main]

// The CBOR reader itself, included directly — no wit bindings dependency.
#[path = "../../src/cbor.rs"]
mod cbor;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // `decode` must never report consuming more bytes than given: `parse_auth_data` slices on
    // that invariant with no re-check of its own.
    if let Ok((_, used)) = cbor::decode(data) {
        assert!(used <= data.len(), "decode reported consuming {used} of {} bytes", data.len());
    }
});
