test "identical texts yield empty diff" {
  let u = @differ.unified("x\ny", "x\ny", "a", "b", 3U)
  assert_eq(u, "")
  let ops = @differ.diff_lines("x", "x")
  assert_eq(ops.length(), 1)
  match ops[0] {
    @differ.Equal("x") => ()
    _ => fail("expected Equal(x)")
  }
}

test "edit script classifies lines" {
  let ops = @differ.diff_lines("a\nb\nc", "a\nB\nc")
  assert_eq(ops.length(), 4)
  match ops[0] {
    @differ.Equal("a") => ()
    _ => fail("expected Equal(a)")
  }
  match ops[1] {
    @differ.Delete("b") => ()
    _ => fail("expected Delete(b)")
  }
  match ops[2] {
    @differ.Insert("B") => ()
    _ => fail("expected Insert(B)")
  }
  match ops[3] {
    @differ.Equal("c") => ()
    _ => fail("expected Equal(c)")
  }
}

test "roundtrip apply reproduces target" {
  let cases = [
    ("", "hello"),
    ("hello", ""),
    ("a\nb\nc\nd\ne", "a\nB\nc\nd\ne"),
    ("a\nb\nc\nd\ne", "a\nc\nd\ne\nf"),
    ("one\ntwo\nthree", "zero\none\ntwo\nthree"),
    ("keep\ndrop\nkeep", "keep\nkeep"),
    ("x\ny\nz\n", "x\nY\nz\n"),
    ("line", "totally\ndifferent\ntext"),
  ]
  for c in cases {
    let (a, b) = c
    for ctx in [0U, 1U, 3U] {
      let patch = @differ.unified(a, b, "a", "b", ctx)
      let got = @differ.apply_unified(a, patch).unwrap()
      assert_eq(got, b)
    }
  }
}

test "apply rejects a patch that does not fit" {
  let patch = @differ.unified("a\nb\nc", "a\nB\nc", "a", "b", 1U)
  match @differ.apply_unified("a\nDIFFERENT\nc", patch) {
    Err(@differ.ContextMismatch(_)) => ()
    _ => fail("expected ContextMismatch")
  }
}
