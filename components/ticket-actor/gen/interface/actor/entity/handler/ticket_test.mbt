// Tests for ticket-actor lifecycle

fn make_bytes(s : String) -> FixedArray[Byte] {
  let encoded = @utf8.encode(s)
  FixedArray::makei(encoded.length(), fn(i) { encoded[i] })
}

test "spawn and snapshot initial ticket state" {
  let init_json = "{\"ref\":\"HD-100\",\"subject\":\"Bug\",\"requester\":\"alice\",\"assignee\":\"\",\"priority\":\"high\",\"status\":\"new\"}"
  let spawn_res = on_spawn("HD-100", make_bytes(init_json))
  match spawn_res {
    Ok(_) => ()
    Err(_) => panic()
  }

  let snap = on_snapshot()
  let snap_str = @utf8.decode_lossy(Bytes::from_array(snap)[:])
  assert_eq(snap_str, init_json)
}

test "triage and assign moves new to open" {
  let _ = on_spawn("HD-101", make_bytes("{\"ref\":\"HD-101\",\"subject\":\"T\",\"requester\":\"b\",\"assignee\":\"\",\"priority\":\"low\",\"status\":\"new\"}"))
  
  let triage_msg = make_bytes("{\"type\":\"ChangeState\",\"event\":\"triage\"}")
  match on_message(triage_msg) {
    Ok(_) => ()
    Err(_) => panic()
  }

  let snap1 = @utf8.decode_lossy(Bytes::from_array(on_snapshot())[:])
  assert_true(snap1.contains("\"status\":\"open\""))

  let assign_msg = make_bytes("{\"type\":\"Assign\",\"assignee\":\"bob\"}")
  match on_message(assign_msg) {
    Ok(_) => ()
    Err(_) => panic()
  }

  let snap2 = @utf8.decode_lossy(Bytes::from_array(on_snapshot())[:])
  assert_true(snap2.contains("\"assignee\":\"bob\""))
}

test "transitions: open -> reply -> requester-reply -> solve -> close" {
  let _ = on_spawn("HD-102", make_bytes("{\"ref\":\"HD-102\",\"subject\":\"T\",\"requester\":\"b\",\"assignee\":\"bob\",\"priority\":\"low\",\"status\":\"open\"}"))

  let reply_msg = make_bytes("{\"type\":\"ChangeState\",\"event\":\"reply\"}")
  match on_message(reply_msg) {
    Ok(_) => ()
    Err(_) => panic()
  }
  let s1 = @utf8.decode_lossy(Bytes::from_array(on_snapshot())[:])
  assert_true(s1.contains("\"status\":\"pending\""))

  let req_reply_msg = make_bytes("{\"type\":\"ChangeState\",\"event\":\"requester-reply\"}")
  match on_message(req_reply_msg) {
    Ok(_) => ()
    Err(_) => panic()
  }
  let s2 = @utf8.decode_lossy(Bytes::from_array(on_snapshot())[:])
  assert_true(s2.contains("\"status\":\"open\""))

  let solve_msg = make_bytes("{\"type\":\"ChangeState\",\"event\":\"solve\"}")
  match on_message(solve_msg) {
    Ok(_) => ()
    Err(_) => panic()
  }
  let s3 = @utf8.decode_lossy(Bytes::from_array(on_snapshot())[:])
  assert_true(s3.contains("\"status\":\"solved\""))

  let close_msg = make_bytes("{\"type\":\"ChangeState\",\"event\":\"close\"}")
  match on_message(close_msg) {
    Ok(_) => ()
    Err(_) => panic()
  }
  let s4 = @utf8.decode_lossy(Bytes::from_array(on_snapshot())[:])
  assert_true(s4.contains("\"status\":\"closed\""))

  // adding a message to closed ticket must fail
  let add_msg = make_bytes("{\"type\":\"AddMessage\",\"author\":\"alice\",\"body\":\"hello\",\"internal\":false}")
  match on_message(add_msg) {
    Err(e) => assert_eq(e, "ticket is closed")
    Ok(_) => panic()
  }
}

test "invalid state transition returns error" {
  let _ = on_spawn("HD-103", make_bytes("{\"ref\":\"HD-103\",\"subject\":\"T\",\"requester\":\"b\",\"assignee\":\"bob\",\"priority\":\"low\",\"status\":\"new\"}"))
  let invalid_msg = make_bytes("{\"type\":\"ChangeState\",\"event\":\"close\"}")
  match on_message(invalid_msg) {
    Err(e) => assert_true(e.contains("invalid transition"))
    Ok(_) => panic()
  }
}
