// Tests for actor-entity handler

test "on_spawn returns Ok" {
  let res = on_spawn("test-1", FixedArray::make(0, b'\x00'))
  match res {
    Ok(_) => ()
    Err(_) => panic()
  }
}

test "on_message returns Ok" {
  let res = on_message(FixedArray::make(0, b'\x00'))
  match res {
    Ok(_) => ()
    Err(_) => panic()
  }
}

test "on_snapshot returns empty byte array" {
  let snap = on_snapshot()
  assert_eq(snap.length(), 0)
}
