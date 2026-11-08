test "grade empty and normal" {
  let res0 = @grader.grade([], [], 50U)
  assert_eq(res0.correct, 0U)
  assert_eq(res0.total, 0U)
  assert_eq(res0.score_pct, 0U)
  assert_eq(res0.passed, false)

  let answers : FixedArray[UInt] = [1U, 2U, 3U, 4U]
  let key : FixedArray[UInt] = [1U, 2U, 0U, 4U]
  let res1 = @grader.grade(answers, key, 70U)
  assert_eq(res1.correct, 3U)
  assert_eq(res1.total, 4U)
  assert_eq(res1.score_pct, 75U)
  assert_eq(res1.passed, true)
}

test "distribution calculation" {
  let d0 = @grader.distribution([], 50U)
  assert_eq(d0.count, 0U)

  let scores : FixedArray[UInt] = [10U, 25U, 50U, 75U, 90U, 100U]
  let d = @grader.distribution(scores, 60U)
  assert_eq(d.count, 6U)
  assert_eq(d.min, 10U)
  assert_eq(d.max, 100U)
  assert_eq(d.pass_count, 3U)
  // mean: (350 + 3) / 6 = 58
  assert_eq(d.mean, 58U)
  // median: 6 elements, (50 + 75 + 1) / 2 = 63
  assert_eq(d.median, 63U)
  // buckets: 10->0, 25->1, 50->2, 75->3, 90->4, 100->4
  assert_eq(d.buckets[0], 1U)
  assert_eq(d.buckets[1], 1U)
  assert_eq(d.buckets[2], 1U)
  assert_eq(d.buckets[3], 1U)
  assert_eq(d.buckets[4], 2U)
}
