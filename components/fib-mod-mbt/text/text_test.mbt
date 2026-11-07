///|
fn half_crab() -> String {
  let crab = "🦀"
  String::from_array([crab.at(0).to_int().unsafe_to_char()])
}

///|
test "an unpaired surrogate becomes U+FFFD and a pair is kept" {
  let broken = "ab" + half_crab()
  assert_false(is_well_formed(broken))
  assert_eq(well_formed(broken), "ab\u{FFFD}")
  assert_eq(well_formed(half_crab() + "🦀x"), "\u{FFFD}🦀x")
  assert_true(is_well_formed("Grüße 🦀"))
  assert_eq(well_formed("Grüße 🦀"), "Grüße 🦀")
}

///|
test "utf8 never aborts and is not to_bytes" {
  assert_eq(utf8("é"), b"\xc3\xa9")
  assert_eq(utf8("a" + half_crab()), b"a\xef\xbf\xbd")
  assert_eq("é".to_bytes(), b"\xe9\x00")
}

///|
test "take_chars cuts between characters" {
  assert_eq(take_chars("ab🦀cd", 3), "ab🦀")
  assert_eq(take_chars("ab", 5), "ab")
  assert_eq(take_chars("ab🦀", 0), "")
}
