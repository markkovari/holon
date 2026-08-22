#[allow(warnings)]
mod bindings;
use bindings::exports::media::video::ffmpeg::Guest;
struct Component;
impl Guest for Component { fn transcode(input: String) -> String { format!("transcoded_{}.mp4", input) } }
bindings::export!(Component with_types_in bindings);
