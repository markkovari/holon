test "escape covers every character that can start markup" {
  let doc = "<script> & \"quoted\" 'it's' &lt; plain"
  let html = @renderer.to_html(doc)
  assert_eq(html.contains("&lt;script&gt;"), true)
  assert_eq(html.contains("&amp;"), true)
  assert_eq(html.contains("&quot;quoted&quot;"), true)
  assert_eq(html.contains("&#x27;"), true)
}

test "only allow-listed schemes can become an href" {
  let safe1 = "[ok](https://example.com)"
  assert_eq(@renderer.to_html(safe1).contains("<a href=\"https://example.com\" rel=\"nofollow noopener\" target=\"_blank\">ok</a>"), true)

  let safe2 = "[ok](http://example.com)"
  assert_eq(@renderer.to_html(safe2).contains("<a href=\"http://example.com\""), true)

  let safe3 = "[ok](mailto:a@b.com)"
  assert_eq(@renderer.to_html(safe3).contains("<a href=\"mailto:a@b.com\""), true)

  let safe4 = "[ok](/relative/path)"
  assert_eq(@renderer.to_html(safe4).contains("<a href=\"/relative/path\""), true)

  let safe5 = "[ok](#anchor)"
  assert_eq(@renderer.to_html(safe5).contains("<a href=\"#anchor\""), true)

  let bad1 = "[bad](javascript:alert(1))"
  assert_eq(@renderer.to_html(bad1).contains("<a href="), false)

  let bad2 = "[bad](data:text/html,<script>)"
  assert_eq(@renderer.to_html(bad2).contains("<a href="), false)

  let bad3 = "[bad](vbscript:msgbox)"
  assert_eq(@renderer.to_html(bad3).contains("<a href="), false)

  let bad4 = "[bad](file:///etc/passwd)"
  assert_eq(@renderer.to_html(bad4).contains("<a href="), false)

  let bad5 = "[bad](ftp://example.com)"
  assert_eq(@renderer.to_html(bad5).contains("<a href="), false)
}

test "scheme checks survive case and control character evasion" {
  assert_eq(@renderer.to_html("[bad](JaVaScRiPt:alert(1))").contains("<a href="), false)
  assert_eq(@renderer.to_html("[bad](JAVASCRIPT:alert(1))").contains("<a href="), false)
  assert_eq(@renderer.to_html("[bad](java\tscript:alert(1))").contains("<a href="), false)
  assert_eq(@renderer.to_html("[bad](java\nscript:alert(1))").contains("<a href="), false)
  assert_eq(@renderer.to_html("[bad](https://exa\tmple.com)").contains("<a href="), false)
  assert_eq(@renderer.to_html("[ok](HTTPS://EXAMPLE.COM)").contains("<a href="), true)
}

test "surrounding whitespace in links" {
  assert_eq(@renderer.to_html("[ok](  https://example.com  )").contains("<a href=\"https://example.com\""), true)
  assert_eq(@renderer.to_html("[bad](   javascript:alert(1)   )").contains("<a href="), false)
}

test "headings lists blockquotes and code blocks" {
  let md = "# Heading 1\n\n> A quote\n\n- Item 1\n- Item 2\n\n```rust\nfn main() {}\n```"
  let html = @renderer.to_html(md)
  assert_eq(html.contains("<h1>Heading 1</h1>\n"), true)
  assert_eq(html.contains("<blockquote><p>A quote</p></blockquote>\n"), true)
  assert_eq(html.contains("<ul>\n<li>Item 1</li>\n<li>Item 2</li>\n</ul>\n"), true)
  assert_eq(html.contains("<pre><code class=\"language-rust\">fn main() {}\n</code></pre>\n"), true)

  let text = @renderer.to_text(md)
  assert_eq(text.contains("Heading 1"), true)
  assert_eq(text.contains("A quote"), true)
  assert_eq(text.contains("Item 1"), true)
}
