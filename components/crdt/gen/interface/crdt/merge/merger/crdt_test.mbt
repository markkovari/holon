// CRDT convergence and mutation tests.

fn fold_merge(states : Array[String]) -> String {
  let mut acc = states[0]
  for i = 1; i < states.length(); i = i + 1 {
    match merge(acc, states[i]) {
      Ok(m) => acc = m
      Err(_) => panic()
    }
  }
  acc
}

fn assert_converges(states : Array[String]) -> Unit {
  let expected = fold_merge(states)
  let idx = [
    [0, 1, 2],
    [0, 2, 1],
    [1, 0, 2],
    [1, 2, 0],
    [2, 0, 1],
    [2, 1, 0],
  ]
  for i = 0; i < idx.length(); i = i + 1 {
    let order = idx[i]
    let permuted = [states[order[0]], states[order[1]], states[order[2]]]
    if fold_merge(permuted) != expected {
      panic()
    }
  }
  match merge(expected, expected) {
    Ok(m) => if m != expected { panic() }
    Err(_) => panic()
  }
}

test "lww higher stamp wins commutatively" {
  let a = "{\"type\":\"lww\",\"v\":\"old\",\"ts\":1,\"replica\":\"A\"}"
  let b = "{\"type\":\"lww\",\"v\":\"new\",\"ts\":2,\"replica\":\"B\"}"
  match merge(a, b) {
    Ok(ab) => {
      let v = value(ab).unwrap()
      assert_eq(v, "\"new\"")
      let ba = merge(b, a).unwrap()
      assert_eq(ab, ba)
    }
    Err(_) => panic()
  }
}

test "pn counter sums and converges" {
  let a = "{\"type\":\"pn\",\"p\":{\"A\":5},\"n\":{}}"
  let b = "{\"type\":\"pn\",\"p\":{\"B\":4},\"n\":{\"B\":2}}"
  let c = "{\"type\":\"pn\",\"p\":{},\"n\":{\"C\":1}}"
  assert_converges([a, b, c])
  let merged = fold_merge([a, b, c])
  assert_eq(value(merged).unwrap(), "6")
}

test "orset add wins over concurrent remove" {
  let removed = "{\"type\":\"orset\",\"adds\":{\"x\":[\"A:1\"]},\"removes\":[\"A:1\"]}"
  let readded = "{\"type\":\"orset\",\"adds\":{\"x\":[\"A:1\",\"B:1\"]},\"removes\":[]}"
  let empty = "{\"type\":\"orset\",\"adds\":{},\"removes\":[]}"
  let merged = merge(removed, readded).unwrap()
  assert_eq(value(merged).unwrap(), "[\"x\"]")
  assert_converges([removed, readded, empty])
}

test "lwwmap converges and tombstone wins" {
  let setter = "{\"type\":\"lwwmap\",\"entries\":{\"k\":{\"v\":\"v\",\"ts\":1,\"replica\":\"A\"}}}"
  let remover = "{\"type\":\"lwwmap\",\"entries\":{\"k\":{\"v\":null,\"ts\":2,\"replica\":\"B\"}}}"
  let other = "{\"type\":\"lwwmap\",\"entries\":{\"j\":{\"v\":\"j\",\"ts\":1,\"replica\":\"C\"}}}"
  assert_converges([setter, remover, other])
  let merged = fold_merge([setter, remover, other])
  assert_eq(value(merged).unwrap(), "{\"j\":\"j\"}")
}

test "different types do not merge" {
  let pn = "{\"type\":\"pn\",\"p\":{},\"n\":{}}"
  let set = "{\"type\":\"orset\",\"adds\":{},\"removes\":[]}"
  match merge(pn, set) {
    Err(TypeMismatch(_)) => ()
    _ => panic()
  }
}

fn rga_ins(state : String, index : UInt, t : String, id : String) -> String {
  match rga_insert(state, index, t, id) {
    Ok(s) => s
    Err(_) => panic()
  }
}

test "rga builds and edits text" {
  let a = rga_ins(rga_new(), 0U, "hello", "0001-a")
  assert_eq(value(a).unwrap(), "\"hello\"")
  let a = rga_ins(a, 5U, " world", "0002-a")
  assert_eq(value(a).unwrap(), "\"hello world\"")
  let a = rga_ins(a, 0U, "say ", "0003-a")
  assert_eq(value(a).unwrap(), "\"say hello world\"")
  let a = rga_delete(a, 0U, 4U).unwrap()
  assert_eq(value(a).unwrap(), "\"hello world\"")
}

test "rga concurrent inserts both survive and converge" {
  let base = rga_ins(rga_new(), 0U, "AC", "0000-seed")
  let rx = rga_ins(base, 1U, "X", "0001-x")
  let ry = rga_ins(base, 1U, "Y", "0002-y")
  let merged_xy = merge(rx, ry).unwrap()
  assert_eq(value(merged_xy).unwrap(), "\"AYXC\"")
  let merged_yx = merge(ry, rx).unwrap()
  assert_eq(merged_xy, merged_yx)
  assert_converges([rx, ry, base])
}

test "rga id anchored ops are unambiguous" {
  let base = rga_ins(rga_new(), 0U, "AC", "0000-seed")
  let elems_json = rga_elements(base).unwrap()
  assert_eq(elems_json.contains("0000-seed:0000"), true)

  let id_a = "0000-seed:0000"
  let rx = rga_insert_after(base, id_a, "X", "0001-x").unwrap()
  let ry = rga_insert_after(base, id_a, "Y", "0002-y").unwrap()
  let merged = merge(rx, ry).unwrap()
  assert_eq(value(merged).unwrap(), "\"AYXC\"")

  let del = rga_delete_ids(base, [id_a]).unwrap()
  assert_eq(value(del).unwrap(), "\"C\"")

  match rga_insert_after(base, "nope", "z", "9-z") {
    Err(InvalidState(_)) => ()
    _ => panic()
  }
}

test "rga delete and concurrent edit both apply" {
  let base = rga_ins(rga_new(), 0U, "abc", "0000-s")
  let deleted = rga_delete(base, 1U, 1U).unwrap()
  let edited = rga_ins(base, 3U, "d", "0001-e")
  let merged = merge(deleted, edited).unwrap()
  assert_eq(value(merged).unwrap(), "\"acd\"")
  assert_converges([deleted, edited, base])
}
