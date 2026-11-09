test "slugify basics" {
  assert_eq(@generator.slugify("Hello World"), "hello-world")
  assert_eq(@generator.slugify("  Lots   of   spaces  "), "lots-of-spaces")
  assert_eq(@generator.slugify("Café & Restaurant!"), "cafe-restaurant")
  assert_eq(@generator.slugify("Schöne Grüße"), "schone-grusse")
}

test "slugify_with custom options" {
  let opts : @generator.Options = { separator: "_", max_length: 0U }
  assert_eq(@generator.slugify_with("Hello World", opts), "hello_world")

  let bound_opts : @generator.Options = { separator: "-", max_length: 12U }
  assert_eq(@generator.slugify_with("Hello World Wonderful Day", bound_opts), "hello-world")
}

test "uniquify" {
  let taken = ["hello-world", "hello-world-2"]
  assert_eq(@generator.uniquify("hello-world", taken), "hello-world-3")
  assert_eq(@generator.uniquify("fresh-slug", taken), "fresh-slug")
}
