test "pdf rendering empty doc" {
  let doc : @codec.Document = { title: "", blocks: [] }
  let bytes = @codec.render(doc)
  assert_eq(bytes.length() > 0, true)

  // Check header
  assert_eq(bytes[0].to_int(), '%'.to_int())
  assert_eq(bytes[1].to_int(), 'P'.to_int())
  assert_eq(bytes[2].to_int(), 'D'.to_int())
  assert_eq(bytes[3].to_int(), 'F'.to_int())
  assert_eq(bytes[4].to_int(), '-'.to_int())
  assert_eq(bytes[5].to_int(), '1'.to_int())
  assert_eq(bytes[6].to_int(), '.'.to_int())
  assert_eq(bytes[7].to_int(), '4'.to_int())

  // Check footer ends with %%EOF
  let len = bytes.length()
  assert_eq(bytes[len - 5].to_int(), '%'.to_int())
  assert_eq(bytes[len - 4].to_int(), '%'.to_int())
  assert_eq(bytes[len - 3].to_int(), 'E'.to_int())
  assert_eq(bytes[len - 2].to_int(), 'O'.to_int())
  assert_eq(bytes[len - 1].to_int(), 'F'.to_int())
}

test "pdf rendering with title and content blocks" {
  let doc : @codec.Document = {
    title: "Quarterly Report",
    blocks: [
      { text: "Introduction (Executive Summary)", size: 14U, bold: true, gap_before: 10U },
      { text: "Total Revenue: €1,200,000 — Up 15%", size: 11U, bold: false, gap_before: 5U },
      { text: "Special chars: (test) \\ slash", size: 11U, bold: false, gap_before: 0U },
    ],
  }
  let bytes = @codec.render(doc)
  assert_eq(bytes.length() > 500, true)
}

test "pdf pagination spans multiple pages" {
  let blocks : Array[@codec.Block] = []
  for i = 0; i < 60; i = i + 1 {
    blocks.push({ text: "Line number \{i}", size: 12U, bold: false, gap_before: 5U })
  }
  let doc : @codec.Document = {
    title: "Multi-page Document",
    blocks,
  }
  let bytes = @codec.render(doc)
  assert_eq(bytes.length() > 1000, true)
}
