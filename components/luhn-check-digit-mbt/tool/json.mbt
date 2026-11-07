///|
/// What both doors (`holon:tool/tool` over wRPC, `wasi:http/handler` over HTTP) call: JSON text in,
/// JSON text out. Fixed; `tool.mbt` is the file a tool's author writes. The result is made
/// well-formed here, so a string cut mid-character costs a U+FFFD instead of the call.
pub fn call_json(args : String) -> Result[String, String] {
  let args = @json.parse(if args.is_empty() { "{}" } else { args }) catch {
    e => return Err("the arguments are not JSON: \{e}")
  }
  match call(args) {
    Ok(out) => Ok(@text.well_formed(out.stringify()))
    Err(e) => Err(@text.well_formed(e))
  }
}
