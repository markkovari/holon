///|
/// The tool: the one file an agent writes. `args` is the call's JSON; `Err` reaches the model as the error.
pub fn call(args : Json) -> Result[Json, String] {
  guard args is { "text": String(text), .. } else {
    return Err("text is required")
  }
  let words = text.split(" ").filter(w => !w.is_empty()).count()
  let lines = text.split("\n").count()
  Ok({
    "chars": Json::number(text.char_length().to_double()),
    "words": Json::number(words.to_double()),
    "lines": Json::number(lines.to_double()),
  })
}
