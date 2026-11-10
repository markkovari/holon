// Implementation of email:template/renderer

let bucket_name = "default"

fn open_bucket() -> Result[@kv.Bucket, RenderError] {
  match @kv.open(bucket_name) {
    Ok(b) => Ok(b)
    Err(_) => Err(BackendUnavailable("open failed"))
  }
}

fn load(bucket : @kv.Bucket, key : String) -> Result[Template, RenderError] {
  match bucket.get(key) {
    Ok(Some(bytes)) => {
      let s = @engine.bytes_to_str(bytes)
      match @engine.parse_template(s) {
        Some((subject, text, html)) => Ok({ subject, text, html })
        None => Err(BackendUnavailable("corrupt template"))
      }
    }
    Ok(None) => Err(UnknownTemplate)
    Err(_) => Err(BackendUnavailable("get failed"))
  }
}

///|
pub fn put_template(name : String, tmpl : Template) -> Result[Unit, RenderError] {
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  let key = @engine.tmpl_key(name)
  let s = @engine.serialize_template(tmpl.subject, tmpl.text, tmpl.html)
  let bytes = @engine.str_to_bytes(s)
  match bucket.set(key, bytes) {
    Ok(_) => Ok(())
    Err(_) => Err(BackendUnavailable("set failed"))
  }
}

///|
pub fn get_template(name : String) -> Result[Template, RenderError] {
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  load(bucket, @engine.tmpl_key(name))
}

///|
pub fn render(name : String, vars : Array[Var]) -> Result[Message, RenderError] {
  let bucket = match open_bucket() {
    Ok(b) => b
    Err(e) => return Err(e)
  }
  let tmpl = match load(bucket, @engine.tmpl_key(name)) {
    Ok(t) => t
    Err(e) => return Err(e)
  }
  let var_pairs = []
  for v in vars {
    var_pairs.push((v.name, v.value))
  }
  let subject = match @engine.render_field(tmpl.subject, var_pairs, false) {
    Ok(s) => s
    Err(missing) => return Err(MissingVariable(missing))
  }
  let text = match @engine.render_field(tmpl.text, var_pairs, false) {
    Ok(t) => t
    Err(missing) => return Err(MissingVariable(missing))
  }
  let html = match @engine.render_field(tmpl.html, var_pairs, true) {
    Ok(h) => h
    Err(missing) => return Err(MissingVariable(missing))
  }
  Ok({ subject, text, html })
}
