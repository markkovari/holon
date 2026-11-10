// Stateful actor representing a helpdesk ticket lifecycle

pub(all) struct TicketState {
  mut id : String
  mut subject : String
  mut requester : String
  mut assignee : String
  mut priority : String
  mut status : String
}

let current_state : Ref[TicketState?] = { val: None }

fn escape_str(s : String) -> String {
  let buf = StringBuilder()
  buf.write_char('"')
  for c in s {
    match c {
      '"' => buf.write_string("\\\"")
      '\\' => buf.write_string("\\\\")
      '\n' => buf.write_string("\\n")
      '\r' => buf.write_string("\\r")
      '\t' => buf.write_string("\\t")
      _ => {
        let code = c.to_int()
        if code < 32 {
          buf.write_string("\\u00")
          let h1 = code / 16
          let h2 = code % 16
          let hex_c = fn(n : Int) -> Char {
            if n < 10 {
              (n + 48).unsafe_to_char()
            } else {
              (n - 10 + 97).unsafe_to_char()
            }
          }
          buf.write_char(hex_c(h1))
          buf.write_char(hex_c(h2))
        } else {
          buf.write_char(c)
        }
      }
    }
  }
  buf.write_char('"')
  buf.to_string()
}

///|
pub fn on_spawn(_id : String, state_bytes : FixedArray[Byte]) -> Result[Unit, String] {
  let text = @utf8.decode_lossy(Bytes::from_array(state_bytes)[:])
  let root = try {
    @json.parse(text)
  } catch {
    e => return Err("failed to deserialize state: \{e}")
  }
  let map = match root {
    Object(m) => m
    _ => return Err("failed to deserialize state: expected json object")
  }
  let id = match map.get("ref") {
    Some(String(s)) => s
    _ => ""
  }
  let subject = match map.get("subject") {
    Some(String(s)) => s
    _ => ""
  }
  let requester = match map.get("requester") {
    Some(String(s)) => s
    _ => ""
  }
  let assignee = match map.get("assignee") {
    Some(String(s)) => s
    _ => ""
  }
  let priority = match map.get("priority") {
    Some(String(s)) => s
    _ => ""
  }
  let status = match map.get("status") {
    Some(String(s)) => s
    _ => ""
  }
  current_state.val = Some({
    id,
    subject,
    requester,
    assignee,
    priority,
    status,
  })
  Ok(())
}

///|
pub fn on_message(message_bytes : FixedArray[Byte]) -> Result[Unit, String] {
  let text = @utf8.decode_lossy(Bytes::from_array(message_bytes)[:])
  let root = try {
    @json.parse(text)
  } catch {
    e => return Err("failed to deserialize message: \{e}")
  }
  let map = match root {
    Object(m) => m
    _ => return Err("failed to deserialize message: expected json object")
  }

  let state = match current_state.val {
    Some(s) => s
    None => return Err("actor not initialized")
  }

  let msg_type = match map.get("type") {
    Some(String(t)) => t
    _ => return Err("missing message type")
  }

  match msg_type {
    "AddMessage" => {
      if state.status == "closed" {
        return Err("ticket is closed")
      }
      Ok(())
    }
    "ChangeState" => {
      let event = match map.get("event") {
        Some(String(e)) => e
        _ => return Err("missing event in ChangeState message")
      }
      let new_status = match (state.status, event) {
        ("new", "triage") => "open"
        ("open", "reply") => "pending"
        ("pending", "requester-reply") => "open"
        ("solved", "reopen") => "open"
        ("new", "solve") | ("open", "solve") | ("pending", "solve") => "solved"
        ("solved", "close") => "closed"
        (s, e) => return Err("invalid transition \{s} -> \{e}")
      }
      state.status = new_status
      Ok(())
    }
    "Assign" => {
      let assignee = match map.get("assignee") {
        Some(String(a)) => a
        _ => return Err("missing assignee in Assign message")
      }
      state.assignee = assignee
      if state.status == "new" {
        state.status = "open"
      }
      Ok(())
    }
    other => Err("unknown message type: \{other}")
  }
}

///|
pub fn on_snapshot() -> FixedArray[Byte] {
  match current_state.val {
    None => FixedArray::make(0, b'\x00')
    Some(s) => {
      let json_string = "{\"ref\":\{escape_str(s.id)},\"subject\":\{escape_str(s.subject)},\"requester\":\{escape_str(s.requester)},\"assignee\":\{escape_str(s.assignee)},\"priority\":\{escape_str(s.priority)},\"status\":\{escape_str(s.status)}}"
      let encoded = @utf8.encode(json_string)
      FixedArray::makei(encoded.length(), fn(i) { encoded[i] })
    }
  }
}
