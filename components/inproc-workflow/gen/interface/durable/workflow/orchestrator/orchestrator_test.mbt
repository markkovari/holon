// Unit tests for inproc-workflow orchestrator

test "email resize report and echo workflows succeed" {
  match trigger({ workflow_id: "email", payload: "{}" }) {
    Ok(v) => assert_eq(v, "{\"sent\":true}")
    Err(_) => panic()
  }

  match trigger({ workflow_id: "resize", payload: "{}" }) {
    Ok(v) => assert_eq(v, "{\"resized\":true}")
    Err(_) => panic()
  }

  match trigger({ workflow_id: "report", payload: "{}" }) {
    Ok(v) => assert_eq(v, "{\"rows\":42}")
    Err(_) => panic()
  }

  match trigger({ workflow_id: "echo", payload: "{\"hello\":\"world\"}" }) {
    Ok(v) => assert_eq(v, "{\"hello\":\"world\"}")
    Err(_) => panic()
  }
}

test "unknown workflow returns not found" {
  match trigger({ workflow_id: "nonexistent", payload: "{}" }) {
    Err(NotFound(id)) => assert_eq(id, "nonexistent")
    _ => panic()
  }
}

test "flaky workflow fails before fail_until and succeeds after" {
  match trigger({ workflow_id: "flaky", payload: "{\"attempt\":1,\"fail_until\":3}" }) {
    Err(WorkerFailed(msg)) => assert_eq(msg, "transient failure (attempt 1 < 3)")
    _ => panic()
  }

  match trigger({ workflow_id: "flaky", payload: "{\"attempt\":2,\"fail_until\":3}" }) {
    Err(WorkerFailed(msg)) => assert_eq(msg, "transient failure (attempt 2 < 3)")
    _ => panic()
  }

  match trigger({ workflow_id: "flaky", payload: "{\"attempt\":3,\"fail_until\":3}" }) {
    Ok(v) => assert_eq(v, "{\"ok_after\":3}")
    _ => panic()
  }
}

test "boom workflow fails permanently" {
  match trigger({ workflow_id: "boom", payload: "{}" }) {
    Err(WorkerFailed(msg)) => assert_eq(msg, "permanent failure")
    _ => panic()
  }
}

test "invalid json payload returns invalid input error" {
  match trigger({ workflow_id: "email", payload: "not json at all" }) {
    Err(InvalidInput(_)) => ()
    _ => panic()
  }
}

test "start and status return unavailable" {
  match start({ workflow_id: "email", payload: "{}" }) {
    Err(Unavailable(_)) => ()
    _ => panic()
  }

  match status("run-123") {
    Err(Unavailable(_)) => ()
    _ => panic()
  }
}
