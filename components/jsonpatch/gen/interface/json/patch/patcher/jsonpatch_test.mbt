test "pointer unescaping" {
  // RFC 6901 §4: ~1 before ~0; reversed, ~01 decodes to / instead of ~1.
  let doc = "{\"a/b~c\": 1, \"~1\": 2, \"plain\": 3}"
  let patch = "[{\"op\": \"test\", \"path\": \"/a~1b~0c\", \"value\": 1}, {\"op\": \"test\", \"path\": \"/~01\", \"value\": 2}]"
  match @patcher.apply_patch(doc, patch) {
    Ok(_) => ()
    Err(_) => fail("unescaping test failed")
  }
}

test "rfc 6902 patch operations" {
  let doc = "{\"foo\": \"bar\", \"numbers\": [1, 2, 3]}"

  // Add
  let patch_add = "[{\"op\": \"add\", \"path\": \"/baz\", \"value\": \"qux\"}, {\"op\": \"add\", \"path\": \"/numbers/1\", \"value\": 99}]"
  match @patcher.apply_patch(doc, patch_add) {
    Ok(res) => {
      let parsed = @json.parse(res) catch { _ => fail("bad json") }
      match parsed {
        Object(m) => {
          assert_eq(m.get("baz") is Some(String("qux")), true)
          match m.get("numbers") {
            Some(Array(arr)) => {
              assert_eq(arr.length(), 4)
              assert_eq(arr[1], @json.Json::number(99.0))
            }
            _ => fail("numbers not array")
          }
        }
        _ => fail("not object")
      }
    }
    Err(_) => fail("apply_patch add failed")
  }

  // Remove
  let patch_rm = "[{\"op\": \"remove\", \"path\": \"/foo\"}]"
  match @patcher.apply_patch(doc, patch_rm) {
    Ok(res) => {
      let parsed = @json.parse(res) catch { _ => fail("bad json") }
      match parsed {
        Object(m) => assert_eq(m.get("foo") is None, true)
        _ => fail("not object")
      }
    }
    Err(_) => fail("apply_patch rm failed")
  }

  // Replace
  let patch_rep = "[{\"op\": \"replace\", \"path\": \"/foo\", \"value\": 42}]"
  match @patcher.apply_patch(doc, patch_rep) {
    Ok(res) => {
      let parsed = @json.parse(res) catch { _ => fail("bad json") }
      match parsed {
        Object(m) => assert_eq(m.get("foo") is Some(Number(42.0, ..)), true)
        _ => fail("not object")
      }
    }
    Err(_) => fail("apply_patch replace failed")
  }

  // Move
  let patch_mv = "[{\"op\": \"move\", \"from\": \"/foo\", \"path\": \"/moved\"}]"
  match @patcher.apply_patch(doc, patch_mv) {
    Ok(res) => {
      let parsed = @json.parse(res) catch { _ => fail("bad json") }
      match parsed {
        Object(m) => {
          assert_eq(m.get("foo") is None, true)
          assert_eq(m.get("moved") is Some(String("bar")), true)
        }
        _ => fail("not object")
      }
    }
    Err(_) => fail("apply_patch move failed")
  }

  // Copy
  let patch_cp = "[{\"op\": \"copy\", \"from\": \"/foo\", \"path\": \"/copied\"}]"
  match @patcher.apply_patch(doc, patch_cp) {
    Ok(res) => {
      let parsed = @json.parse(res) catch { _ => fail("bad json") }
      match parsed {
        Object(m) => {
          assert_eq(m.get("foo") is Some(String("bar")), true)
          assert_eq(m.get("copied") is Some(String("bar")), true)
        }
        _ => fail("not object")
      }
    }
    Err(_) => fail("apply_patch copy failed")
  }

  // Test success & failure
  let patch_test_ok = "[{\"op\": \"test\", \"path\": \"/foo\", \"value\": \"bar\"}]"
  assert_eq(@patcher.apply_patch(doc, patch_test_ok) is Ok(_), true)

  let patch_test_fail = "[{\"op\": \"test\", \"path\": \"/foo\", \"value\": \"wrong\"}]"
  match @patcher.apply_patch(doc, patch_test_fail) {
    Err(TestFailed(_)) => ()
    _ => fail("expected TestFailed")
  }
}

test "rfc 7386 merge-patch and diff" {
  let doc = "{\"title\": \"Hello\", \"author\": {\"givenName\": \"John\", \"familyName\": \"Doe\"}, \"tags\": [\"a\", \"b\"]}"
  let merge_patch = "{\"title\": \"Hello World\", \"author\": {\"givenName\": null}}"
  match @patcher.apply_merge(doc, merge_patch) {
    Ok(res) => {
      let parsed = @json.parse(res) catch { _ => fail("bad json") }
      match parsed {
        Object(m) => {
          assert_eq(m.get("title") is Some(String("Hello World")), true)
          match m.get("author") {
            Some(Object(auth)) => {
              assert_eq(auth.get("givenName") is None, true)
              assert_eq(auth.get("familyName") is Some(String("Doe")), true)
            }
            _ => fail("author not object")
          }
        }
        _ => fail("not object")
      }
    }
    Err(_) => fail("apply_merge failed")
  }

  // Diff roundtrip
  let source = "{\"a\": 1, \"b\": 2, \"c\": 3}"
  let target = "{\"a\": 1, \"b\": 99, \"d\": 4}"
  match @patcher.diff(source, target) {
    Ok(diff_str) => {
      match @patcher.apply_merge(source, diff_str) {
        Ok(merged) => {
          let merged_json = @json.parse(merged) catch { _ => fail("bad json") }
          let target_json = @json.parse(target) catch { _ => fail("bad json") }
          assert_eq(merged_json, target_json)
        }
        Err(_) => fail("merge of diff failed")
      }
    }
    Err(_) => fail("diff failed")
  }
}

test "patch errors" {
  let doc = "{\"a\": 1}"
  // Invalid json
  assert_eq(@patcher.apply_patch("bad-json", "[]") is Err(InvalidJson(_)), true)
  assert_eq(@patcher.apply_patch("{}", "bad-json") is Err(InvalidJson(_)), true)

  // Not an array
  assert_eq(@patcher.apply_patch(doc, "{}") is Err(InvalidPatch(_)), true)

  // Path not found
  assert_eq(@patcher.apply_patch(doc, "[{\"op\": \"remove\", \"path\": \"/nonexistent\"}]") is Err(PathNotFound(_)), true)

  // Invalid pointer
  assert_eq(@patcher.apply_patch(doc, "[{\"op\": \"remove\", \"path\": \"no-slash\"}]") is Err(InvalidPatch(_)), true)
}
