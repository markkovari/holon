test "parse simple csv" {
  let opts = @codec.Dialect::{ delimiter: ",", has_header: true, trim: true }
  let rows = @codec.parse("a,b,c\n1,2,3", opts).unwrap()
  assert_eq(rows.length(), 2)
  assert_eq(rows[0].fields, ["a", "b", "c"])
  assert_eq(rows[1].fields, ["1", "2", "3"])
}

test "parse records with header" {
  let opts = @codec.Dialect::{ delimiter: ",", has_header: true, trim: true }
  let recs = @codec.parse_records("name,age\nAlice,30\nBob,25", opts).unwrap()
  assert_eq(recs.length(), 2)
  assert_eq(recs[0].pairs, [("name", "Alice"), ("age", "30")])
  assert_eq(recs[1].pairs, [("name", "Bob"), ("age", "25")])
}

test "ragged row detection" {
  let opts = @codec.Dialect::{ delimiter: ",", has_header: true, trim: true }
  match @codec.parse_records("a,b\n1,2,3", opts) {
    Err(RaggedRow(0U)) => ()
    _ => fail("expected RaggedRow(0)")
  }
}

test "quotes with embedded commas and escaped quotes" {
  let opts = @codec.Dialect::{ delimiter: ",", has_header: false, trim: false }
  let rows = @codec.parse("\"a,b\",\"c\"\"d\"", opts).unwrap()
  assert_eq(rows.length(), 1)
  assert_eq(rows[0].fields, ["a,b", "c\"d"])
}

test "format rows to csv" {
  let opts = @codec.Dialect::{ delimiter: ",", has_header: false, trim: false }
  let rows = [
    @codec.Row::{ fields: ["hello, world", "plain", "say \"hi\""] },
  ]
  let out = @codec.format(rows, opts)
  assert_eq(out, "\"hello, world\",plain,\"say \"\"hi\"\"\"")
}
