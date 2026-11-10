// Tests for email rendering engine

test "html escaping escapes dangerous characters" {
  let raw = "Hello & Welcome <script>alert(\"xss\")</script> 'test'"
  let escaped = html_escape(raw)
  assert_eq(escaped, "Hello &amp; Welcome &lt;script&gt;alert(&quot;xss&quot;)&lt;/script&gt; &#x27;test&#x27;")
}

test "render_field plain text does not escape" {
  let vars : Array[(String, String)] = [
    ("name", "Bob & Alice"),
  ]
  match render_field("Hello {name}!", vars, false) {
    Ok(rendered) => assert_eq(rendered, "Hello Bob & Alice!")
    Err(_) => panic()
  }
}

test "render_field html escapes" {
  let vars : Array[(String, String)] = [
    ("name", "Bob & Alice <script>"),
  ]
  match render_field("<h1>Hello {name}!</h1>", vars, true) {
    Ok(rendered) => assert_eq(rendered, "<h1>Hello Bob &amp; Alice &lt;script&gt;!</h1>")
    Err(_) => panic()
  }
}

test "render_field missing variable errors" {
  let vars : Array[(String, String)] = [
    ("other", "value"),
  ]
  match render_field("Hello {missing}!", vars, false) {
    Ok(_) => panic()
    Err(missing) => assert_eq(missing, "missing")
  }
}

test "render_field preserves literal braces" {
  let vars : Array[(String, String)] = []
  match render_field("JSON { \"key\": 123 } and {}", vars, false) {
    Ok(rendered) => assert_eq(rendered, "JSON { \"key\": 123 } and {}")
    Err(_) => panic()
  }
}

test "serialize and parse template roundtrip" {
  let s = "Subject: Welcome!"
  let t = "Hello,\nWelcome to the team!\nBest,\nStaff"
  let h = "<h1>Welcome</h1>\n<p>Enjoy your stay</p>"
  let serialized = serialize_template(s, t, h)
  match parse_template(serialized) {
    Some((subj, text, html)) => {
      assert_eq(subj, s)
      assert_eq(text, t)
      assert_eq(html, h)
    }
    None => panic()
  }
}

test "tmpl_key sanitization" {
  assert_eq(tmpl_key("welcome-email"), "tmpl_welcome-email")
  assert_eq(tmpl_key("folder/sub"), "tmpl_folder/sub")
  assert_eq(tmpl_key("hello world"), "tmpl_hello_20world")
}
