pub fn grade(
  answers : FixedArray[UInt],
  key : FixedArray[UInt],
  pass_mark : UInt
) -> GradeResult {
  let total = key.length().reinterpret_as_uint()
  if total == 0U {
    return { correct: 0U, total: 0U, score_pct: 0U, passed: 0U >= pass_mark }
  }
  let mut correct = 0U
  for i = 0; i < key.length(); i = i + 1 {
    if i < answers.length() && answers[i] == key[i] {
      correct += 1U
    }
  }
  let score_pct = (correct * 100U + total / 2U) / total
  let passed = score_pct >= pass_mark
  { correct, total, score_pct, passed }
}

pub fn distribution(scores : FixedArray[UInt], pass_mark : UInt) -> Stats {
  let count = scores.length().reinterpret_as_uint()
  if count == 0U {
    return {
      count: 0U,
      mean: 0U,
      median: 0U,
      min: 0U,
      max: 0U,
      pass_count: 0U,
      buckets: [0U, 0U, 0U, 0U, 0U],
    }
  }

  let mut sum = 0U
  let mut min = scores[0]
  let mut max = scores[0]
  let mut pass_count = 0U
  let buckets : FixedArray[UInt] = [0U, 0U, 0U, 0U, 0U]

  for i = 0; i < scores.length(); i = i + 1 {
    let s = scores[i]
    sum += s
    if s < min {
      min = s
    }
    if s > max {
      max = s
    }
    if s >= pass_mark {
      pass_count += 1U
    }
    let clamped = if s > 100U { 100U } else { s }
    let b = (clamped / 20U).reinterpret_as_int()
    let b_idx = if b > 4 { 4 } else { b }
    buckets[b_idx] += 1U
  }

  let mean = (sum + count / 2U) / count

  // Sort a copy for median
  let sorted = scores.copy()
  sorted.sort()
  let n = sorted.length()
  let median = if n % 2 == 1 {
    sorted[n / 2]
  } else {
    (sorted[n / 2 - 1] + sorted[n / 2] + 1U) / 2U
  }

  { count, mean, median, min, max, pass_count, buckets }
}
