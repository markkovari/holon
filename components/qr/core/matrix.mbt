///|
/// Internal working matrix. `is_function` marks modules that must not be masked
/// and must not receive data bits.
priv struct Matrix {
  size : Int32
  modules : Array[Bool]
  is_function : Array[Bool]
}

///|
fn Matrix::new(size : Int32) -> Matrix {
  {
    size,
    modules: Array::make(size * size, false),
    is_function: Array::make(size * size, false),
  }
}

///|
fn Matrix::set_function(
  self : Matrix,
  x : Int32,
  y : Int32,
  dark : Bool,
) -> Unit {
  let idx = y * self.size + x
  self.modules[idx] = dark
  self.is_function[idx] = true
}

///|
fn Matrix::copy(self : Matrix) -> Matrix {
  {
    size: self.size,
    modules: self.modules.copy(),
    is_function: self.is_function.copy(),
  }
}

///|
fn draw_finder(m : Matrix, cx : Int32, cy : Int32) -> Unit {
  for dy = -4; dy <= 4; dy = dy + 1 {
    for dx = -4; dx <= 4; dx = dx + 1 {
      let x = cx + dx
      let y = cy + dy
      if x >= 0 && x < m.size && y >= 0 && y < m.size {
        let ax = if dx < 0 { -dx } else { dx }
        let ay = if dy < 0 { -dy } else { dy }
        let dist = if ax > ay { ax } else { ay }
        m.set_function(x, y, dist != 2 && dist != 4)
      }
    }
  }
}

///|
fn draw_alignment(m : Matrix, cx : Int32, cy : Int32) -> Unit {
  for dy = -2; dy <= 2; dy = dy + 1 {
    for dx = -2; dx <= 2; dx = dx + 1 {
      let ax = if dx < 0 { -dx } else { dx }
      let ay = if dy < 0 { -dy } else { dy }
      let dist = if ax > ay { ax } else { ay }
      m.set_function(cx + dx, cy + dy, dist != 1)
    }
  }
}

///|
/// ISO Table E.1 centres, computed with the standard spacing rule.
fn alignment_positions(version : Int32, size : Int32) -> Array[Int32] {
  if version == 1 {
    []
  } else {
    let num_align = version / 7 + 2
    let step = if version == 32 {
      26
    } else {
      (version * 4 + num_align * 2 + 1) / (num_align * 2 - 2) * 2
    }
    let result = Array::make(num_align, 0)
    result[0] = 6
    let mut pos = size - 7
    for i = num_align - 1; i >= 1; i = i - 1 {
      result[i] = pos
      pos -= step
    }
    result
  }
}

///|
fn draw_version(m : Matrix, version : Int32) -> Unit {
  if version >= 7 {
    match version_info(version) {
      Some(bits) =>
        for i in 0..<18 {
          let bit = ((bits >> i) & 1) == 1
          let a = m.size - 11 + i % 3
          let b = i / 3
          m.set_function(a, b, bit)
          m.set_function(b, a, bit)
        }
      None => ()
    }
  }
}

///|
fn draw_format_bits(m : Matrix, ec : EcLevel, mask : Mask) -> Unit {
  let bits = format_info(ec, mask)
  let size = m.size
  for i in 0..<=5 {
    m.set_function(8, i, ((bits >> i) & 1) == 1)
  }
  m.set_function(8, 7, ((bits >> 6) & 1) == 1)
  m.set_function(8, 8, ((bits >> 7) & 1) == 1)
  m.set_function(7, 8, ((bits >> 8) & 1) == 1)
  for i in 9..<15 {
    m.set_function(14 - i, 8, ((bits >> i) & 1) == 1)
  }
  for i in 0..<8 {
    m.set_function(size - 1 - i, 8, ((bits >> i) & 1) == 1)
  }
  for i in 8..<15 {
    m.set_function(8, size - 15 + i, ((bits >> i) & 1) == 1)
  }
  m.set_function(8, size - 8, true)
}

///|
fn draw_function_patterns(m : Matrix, version : Int32) -> Unit {
  let size = m.size
  for i in 0..<size {
    let dark = i % 2 == 0
    m.set_function(6, i, dark)
    m.set_function(i, 6, dark)
  }
  draw_finder(m, 3, 3)
  draw_finder(m, size - 4, 3)
  draw_finder(m, 3, size - 4)
  let positions = alignment_positions(version, size)
  let n = positions.length()
  for i in 0..<n {
    for j in 0..<n {
      if !((i == 0 && j == 0) ||
        (i == 0 && j == n - 1) ||
        (i == n - 1 && j == 0)) {
        draw_alignment(m, positions[i], positions[j])
      }
    }
  }
  draw_format_bits(m, EcLevel::M, Mask::M0)
  draw_version(m, version)
}

///|
/// Zig-zag placement, two columns at a time from the right, skipping column 6.
fn place_codewords(m : Matrix, codewords : Array[Byte]) -> Unit {
  let size = m.size
  let total_bits = codewords.length() * 8
  let mut bit_index = 0
  let mut right = size - 1
  while right >= 1 {
    if right == 6 {
      right = 5
    }
    for vert in 0..<size {
      for j in 0..<2 {
        let x = right - j
        let upward = ((right + 1) & 2) == 0
        let y = if upward { size - 1 - vert } else { vert }
        let idx = y * size + x
        if !m.is_function[idx] && bit_index < total_bits {
          let byte = codewords[bit_index / 8].to_int()
          m.modules[idx] = ((byte >> (7 - bit_index % 8)) & 1) == 1
          bit_index += 1
        }
      }
    }
    right -= 2
  }
}

///|
fn mask_applies(mask : Mask, row : Int32, col : Int32) -> Bool {
  match mask {
    M0 => (row + col) % 2 == 0
    M1 => row % 2 == 0
    M2 => col % 3 == 0
    M3 => (row + col) % 3 == 0
    M4 => (row / 2 + col / 3) % 2 == 0
    M5 => row * col % 2 + row * col % 3 == 0
    M6 => (row * col % 2 + row * col % 3) % 2 == 0
    M7 => ((row + col) % 2 + row * col % 3) % 2 == 0
  }
}

///|
/// Applies one mask to data modules and writes the matching format information.
fn apply_mask(base : Matrix, mask : Mask, ec : EcLevel) -> Matrix {
  let result = base.copy()
  for y in 0..<result.size {
    for x in 0..<result.size {
      let idx = y * result.size + x
      if !result.is_function[idx] && mask_applies(mask, y, x) {
        result.modules[idx] = !result.modules[idx]
      }
    }
  }
  draw_format_bits(result, ec, mask)
  result
}

///|
fn choose_best_mask(base : Matrix, ec : EcLevel) -> (Matrix, Mask) {
  let masks : Array[Mask] = [M0, M1, M2, M3, M4, M5, M6, M7]
  let mut best = apply_mask(base, masks[0], ec)
  let mut best_mask = masks[0]
  let mut best_score = penalty(best)
  for i in 1..<8 {
    let candidate = apply_mask(base, masks[i], ec)
    let score = penalty(candidate)
    if score < best_score {
      best = candidate
      best_mask = masks[i]
      best_score = score
    }
  }
  (best, best_mask)
}

///|
/// Builds the unmasked symbol with function patterns and data codewords placed.
fn build_matrix(version : Int32, codewords : Array[Byte]) -> Matrix {
  let size = version * 4 + 17
  let m = Matrix::new(size)
  draw_function_patterns(m, version)
  place_codewords(m, codewords)
  m
}
