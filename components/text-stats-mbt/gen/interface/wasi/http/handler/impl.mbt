///|
/// Bodies past this are refused, so a runaway caller cannot fill the instance's memory.
const MAX_BODY : Int32 = 1048576

///|
pub async fn handle(
  request : @types.Request,
  _background_group : @async-core.TaskGroup[Unit],
) -> Result[@types.Response, @types.ErrorCode] {
  let (status, text) = match read_body(request) {
    Err(e) => (400U, e)
    Ok(body) => answer(body)
  }
  respond(status, text)
}

///|
fn answer(body : Bytes) -> (UInt32, String) {
  let text = @utf8.decode(body) catch {
    _ => return (400U, "the body is not UTF-8")
  }
  match @tool.call_json(text) {
    Ok(out) => (200U, out)
    Err(e) => (422U, e)
  }
}

///|
async fn read_body(request : @types.Request) -> Result[Bytes, String] {
  let (done, _) = @async-core.Future::new()
  let (stream, _) = @types.Request::consume_body(request, done)
  let buf = Buffer()
  while stream.read(65536) is Some(chunk) {
    if buf.length() + chunk.length() > MAX_BODY {
      return Err("the body is over \{MAX_BODY} bytes")
    }
    for b in chunk {
      buf.write_byte(b)
    }
  }
  Ok(buf.to_bytes())
}

///|
fn respond(
  status : UInt32,
  text : String,
) -> Result[@types.Response, @types.ErrorCode] {
  let body = @text.utf8(text)
  let content_type = if status == 200 {
    "application/json"
  } else {
    "text/plain; charset=utf-8"
  }
  let headers = @types.Fields::from_list([
    ("content-type", @text.utf8(content_type).to_fixedarray()),
  ]).unwrap()
  let contents = @async-core.Stream::produce(async fn(sink) {
    ignore(sink.write_all_bytes(body))
    sink.close()
  })
  let (resp, _) = @types.Response::new(
    headers,
    Some(contents),
    @async-core.Future::ready(Ok(None)),
  )
  ignore(resp.set_status_code(status))
  Ok(resp)
}
