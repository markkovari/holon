fn ids(n : Int) -> Array[String] {
  let res : Array[String] = []
  for i = 0; i < n; i = i + 1 {
    res.push("id-\{i}")
  }
  res
}

test "a page is the slice and says whether more remain" {
  let p1 = @pager.paginate(ids(5), 2U, 0U)
  assert_eq(p1.hits, ["id-0", "id-1"])
  assert_eq(p1.has_more, true)

  let p2 = @pager.paginate(ids(5), 2U, 4U)
  assert_eq(p2.hits, ["id-4"])
  assert_eq(p2.has_more, false)
}

test "an offset past the end is empty rather than a panic" {
  let p = @pager.paginate(ids(3), 2U, 99U)
  assert_eq(p.hits.is_empty(), true)
  assert_eq(p.has_more, false)
}

test "a size of zero is an empty page rather than a panic" {
  let p = @pager.paginate(ids(3), 0U, 0U)
  assert_eq(p.hits.is_empty(), true)
  assert_eq(p.has_more, true)
}

test "paging an empty list is empty" {
  let p = @pager.paginate([], 10U, 0U)
  assert_eq(p.hits.is_empty(), true)
  assert_eq(p.has_more, false)
}
