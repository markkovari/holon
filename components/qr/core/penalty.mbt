///|
fn add_run_history(
  run_length : Int32,
  history : Array[Int32],
  size : Int32,
) -> Unit {
  let len = if history[0] == 0 { run_length + size } else { run_length }
  for i = 6; i >= 1; i = i - 1 {
    history[i] = history[i - 1]
  }
  history[0] = len
}

///|
fn count_finder_patterns(history : Array[Int32]) -> Int32 {
  let n = history[1]
  let core = n > 0 &&
    history[2] == n &&
    history[3] == n * 3 &&
    history[4] == n &&
    history[5] == n
  let mut count = 0
  if core && history[0] >= n * 4 && history[6] >= n {
    count += 1
  }
  if core && history[6] >= n * 4 && history[0] >= n {
    count += 1
  }
  count
}

///|
/// N1 (runs) and N3 (finder-like patterns) for one row.
fn penalty_row(m : Matrix, y : Int32) -> Int32 {
  let size = m.size
  let mut result = 0
  let mut run_color = false
  let mut run_len = 0
  let history = Array::make(7, 0)
  for x in 0..<size {
    let color = m.modules[y * size + x]
    if color == run_color {
      run_len += 1
      if run_len == 5 {
        result += 3
      } else if run_len > 5 {
        result += 1
      }
    } else {
      add_run_history(run_len, history, size)
      if !run_color {
        result += count_finder_patterns(history) * 40
      }
      run_color = color
      run_len = 1
    }
  }
  let mut tail = run_len
  if run_color {
    add_run_history(tail, history, size)
    tail = 0
  }
  tail += size
  add_run_history(tail, history, size)
  result + count_finder_patterns(history) * 40
}

///|
/// N1 and N3 for one column.
fn penalty_column(m : Matrix, x : Int32) -> Int32 {
  let size = m.size
  let mut result = 0
  let mut run_color = false
  let mut run_len = 0
  let history = Array::make(7, 0)
  for y in 0..<size {
    let color = m.modules[y * size + x]
    if color == run_color {
      run_len += 1
      if run_len == 5 {
        result += 3
      } else if run_len > 5 {
        result += 1
      }
    } else {
      add_run_history(run_len, history, size)
      if !run_color {
        result += count_finder_patterns(history) * 40
      }
      run_color = color
      run_len = 1
    }
  }
  let mut tail = run_len
  if run_color {
    add_run_history(tail, history, size)
    tail = 0
  }
  tail += size
  add_run_history(tail, history, size)
  result + count_finder_patterns(history) * 40
}

///|
/// ISO Annex J penalty score: N1 runs, N2 blocks, N3 finder-like, N4 balance.
fn penalty(m : Matrix) -> Int32 {
  let size = m.size
  let mut result = 0
  for y in 0..<size {
    result += penalty_row(m, y)
  }
  for x in 0..<size {
    result += penalty_column(m, x)
  }
  for y in 0..<(size - 1) {
    for x in 0..<(size - 1) {
      let idx = y * size + x
      let color = m.modules[idx]
      let same = color == m.modules[idx + 1] &&
        color == m.modules[idx + size] &&
        color == m.modules[idx + size + 1]
      if same {
        result += 3
      }
    }
  }
  let mut dark = 0
  for y in 0..<size {
    for x in 0..<size {
      if m.modules[y * size + x] {
        dark += 1
      }
    }
  }
  let total = size * size
  let diff = if dark * 20 >= total * 10 {
    dark * 20 - total * 10
  } else {
    total * 10 - dark * 20
  }
  let k = (diff + total - 1) / total - 1
  result + k * 10
}
