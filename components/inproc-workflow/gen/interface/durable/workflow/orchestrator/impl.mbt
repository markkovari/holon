// Implementation of durable:workflow/orchestrator in MoonBit

fn is_blank(s : String) -> Bool {
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i]
    if c != ' ' && c != '\t' && c != '\n' && c != '\r' {
      return false
    }
  }
  true
}

///|
pub fn trigger(req : RunRequest) -> Result[String, RunError] {
  let payload_val : @json.Json? = if is_blank(req.payload) {
    None
  } else {
    let parsed = @json.parse(req.payload) catch {
      e => return Err(InvalidInput("payload not JSON: \{e}"))
    }
    Some(parsed)
  }

  match req.workflow_id {
    "email" => Ok("{\"sent\":true}")
    "resize" => Ok("{\"resized\":true}")
    "report" => Ok("{\"rows\":42}")
    "echo" => Ok(req.payload)
    "flaky" => {
      let attempt = match payload_val {
        Some(Object(map)) =>
          match map.get("attempt") {
            Some(Number(n, ..)) => n.to_int64().reinterpret_as_uint64()
            _ => 1UL
          }
        _ => 1UL
      }
      let fail_until = match payload_val {
        Some(Object(map)) =>
          match map.get("fail_until") {
            Some(Number(n, ..)) => n.to_int64().reinterpret_as_uint64()
            _ => 3UL
          }
        _ => 3UL
      }
      if attempt < fail_until {
        Err(WorkerFailed("transient failure (attempt \{attempt} < \{fail_until})"))
      } else {
        Ok("{\"ok_after\":\{attempt}}")
      }
    }
    "boom" => Err(WorkerFailed("permanent failure"))
    other => Err(NotFound(other))
  }
}

///|
pub fn start(_req : RunRequest) -> Result[String, RunError] {
  Err(Unavailable("in-process backend is synchronous; use trigger (async start/status is the Golem backend)"))
}

///|
pub fn status(_run_id : String) -> Result[RunStatus, RunError] {
  Err(Unavailable("in-process backend has no async runs"))
}
