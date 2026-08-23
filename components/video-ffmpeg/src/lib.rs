//! `video-ffmpeg` — re-encode a video into another format or resolution
//!
//! **There is NO implementation behind this contract.** Every export returns
//! an `UNIMPLEMENTED:` marker and `CATALOG.md` lists it as `contract only`.
//!
//! That is the honest state of this component rather than a placeholder
//! someone forgot to fill in, and it cannot be filled in from here: to transcode video needs an ffmpeg process,
//! and a wasm32-wasip2 component has none of those. The contract is the
//! useful part — it states what a host-side implementation must satisfy.
//!
//! It previously returned a plausible-looking constant, which is worse than
//! returning nothing: no caller could tell it apart from a component that
//! works, and neither could a reader of the catalogue. README says "nothing
//! is mocked on the path to a landed change"; this is that rule, applied here.

#[allow(warnings)]
mod bindings;
use bindings::exports::media::video::ffmpeg::Guest;
struct Component;
impl Guest for Component {
    fn transcode(input: String) -> String {
        format!("UNIMPLEMENTED: video-ffmpeg cannot transcode video from wasm ({})", input)
    }
}
bindings::export!(Component with_types_in bindings);
