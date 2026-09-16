#![no_main]

// The CBOR reader itself, included directly — no wit bindings dependency.
#[path = "../../src/cbor.rs"]
mod cbor;

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // The property under test: `decode` must never report having consumed more
    // bytes than it was given — `lib.rs::parse_auth_data` slices
    // `buf[id_end..id_end + used]` on the strength of that invariant, with no
    // re-check of its own.
    if let Ok((_, used)) = cbor::decode(data) {
        assert!(used <= data.len(), "decode reported consuming {used} of {} bytes", data.len());
    }
});
