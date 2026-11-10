// Reference implementation of card:identify/identifier

fn trim_str(s : String) -> String {
  let mut start = 0
  let mut end = s.length()
  while start < end {
    let c = s[start]
    if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
      start = start + 1
    } else {
      break
    }
  }
  while end > start {
    let c = s[end - 1]
    if c == ' ' || c == '\t' || c == '\n' || c == '\r' {
      end = end - 1
    } else {
      break
    }
  }
  s[start:end].to_owned()
}

fn extract_json(answer : String) -> @json.Json? {
  try {
    return Some(@json.parse(answer))
  } catch {
    _ => ()
  }
  let mut start_opt : Int? = None
  for i = 0; i < answer.length(); i = i + 1 {
    if answer[i] == '{' {
      start_opt = Some(i)
      break
    }
  }
  let start = match start_opt {
    Some(s) => s
    None => return None
  }
  let mut depth = 0
  let mut end_opt : Int? = None
  for i = start; i < answer.length(); i = i + 1 {
    let c = answer[i]
    if c == '{' {
      depth = depth + 1
    } else if c == '}' {
      depth = depth - 1
      if depth == 0 {
        end_opt = Some(i)
        break
      }
    }
  }
  let end = match end_opt {
    Some(e) => e
    None => return None
  }
  let sub = answer[start:end + 1].to_owned()
  try {
    Some(@json.parse(sub))
  } catch {
    _ => None
  }
}

fn str_field(map : Map[String, @json.Json], key : String) -> String? {
  match map.get(key) {
    Some(String(s)) => {
      let trimmed = trim_str(s)
      if trimmed.length() == 0 {
        None
      } else {
        Some(trimmed)
      }
    }
    _ => None
  }
}

fn is_all_digits(s : String) -> Bool {
  if s.length() == 0 {
    return false
  }
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i]
    if c < '0' || c > '9' {
      return false
    }
  }
  true
}

fn normalize_number(raw : String?) -> (String, Bool) {
  match raw {
    None => ("", true)
    Some(s) => {
      let buf = StringBuilder()
      for i = 0; i < s.length(); i = i + 1 {
        let c = s[i]
        if c != '#' && c != ' ' {
          buf.write_char((c.to_int()).unsafe_to_char())
        }
      }
      let cleaned = buf.to_string()
      let mut slash_idx = -1
      for i = 0; i < cleaned.length(); i = i + 1 {
        if cleaned[i] == '/' {
          slash_idx = i
          break
        }
      }
      if slash_idx >= 0 {
        let num = cleaned[0:slash_idx].to_owned()
        let total = cleaned[slash_idx + 1:].to_owned()
        if num.length() > 0 && total.length() > 0 && is_all_digits(num) && is_all_digits(total) {
          let width = total.length()
          let mut padded_num = num
          while padded_num.length() < width {
            padded_num = "0" + padded_num
          }
          return (padded_num + "/" + total, false)
        }
      }
      (cleaned, true)
    }
  }
}

fn parse_variant(s : String) -> VariantKind? {
  let lower = trim_str(s.to_lower())
  match lower {
    "holo" | "holofoil" => Some(HOLO)
    "reverse holo" | "reverse holofoil" => Some(REVERSE_HOLO)
    "1st edition" | "first edition" => Some(FIRST_EDITION)
    "shadowless" => Some(SHADOWLESS)
    "normal" => Some(NORMAL)
    "full art" | "alt art" | "secret rare" => Some(SPECIAL)
    _ => None
  }
}

fn parse_condition(s : String) -> Condition? {
  let lower = trim_str(s.to_lower())
  match lower {
    "mint" | "m" => Some(MINT)
    "near mint" | "nm" => Some(NEAR_MINT)
    "lightly played" | "lp" => Some(LIGHTLY_PLAYED)
    "moderately played" | "mp" => Some(MODERATELY_PLAYED)
    "heavily played" | "hp" => Some(HEAVILY_PLAYED)
    "damaged" | "dmg" => Some(DAMAGED)
    _ => None
  }
}

fn parse_tenths(s : String) -> UInt? {
  let mut whole = 0
  let mut frac = 0
  let mut has_dot = false
  let mut frac_digits = 0
  for i = 0; i < s.length(); i = i + 1 {
    let c = s[i]
    if c == '.' {
      if has_dot {
        return None
      }
      has_dot = true
    } else if c >= '0' && c <= '9' {
      let digit = c.to_int() - '0'.to_int()
      if !has_dot {
        whole = whole * 10 + digit
      } else {
        if frac_digits == 0 {
          frac = digit
          frac_digits = 1
        }
      }
    } else {
      return None
    }
  }
  if s.length() == 0 {
    return None
  }
  Some((whole * 10 + frac).reinterpret_as_uint())
}

fn parse_graded(v : @json.Json) -> Grade? {
  match v {
    String(s) => {
      let trimmed = trim_str(s)
      let mut idx_opt : Int? = None
      for i = 0; i < trimmed.length(); i = i + 1 {
        let c = trimmed[i]
        if c >= '0' && c <= '9' {
          idx_opt = Some(i)
          break
        }
      }
      let idx = match idx_opt {
        Some(i) => i
        None => return None
      }
      let grader = trim_str(trimmed[0:idx].to_owned()).to_upper()
      if grader.length() == 0 {
        return None
      }
      let val_str = trim_str(trimmed[idx:].to_owned())
      let tenths = match parse_tenths(val_str) {
        Some(t) => t
        None => return None
      }
      Some({ grader, tenths })
    }
    Object(o) => {
      let grader = match o.get("grader") {
        Some(String(g)) => trim_str(g).to_upper()
        _ => return None
      }
      if grader.length() == 0 {
        return None
      }
      let grade = match o.get("grade") {
        Some(Number(n, ..)) => n
        _ => return None
      }
      let tenths = (grade * 10.0 + 0.5).to_int().reinterpret_as_uint()
      Some({ grader, tenths })
    }
    _ => None
  }
}

fn cmp_str(a : String, b : String) -> Int {
  let min_len = if a.length() < b.length() { a.length() } else { b.length() }
  for i = 0; i < min_len; i = i + 1 {
    let ca = a[i].to_int()
    let cb = b[i].to_int()
    if ca != cb {
      return ca - cb
    }
  }
  a.length() - b.length()
}

fn sort_and_dedup(arr : Array[String]) -> Array[String] {
  for i = 0; i < arr.length(); i = i + 1 {
    for j = i + 1; j < arr.length(); j = j + 1 {
      if cmp_str(arr[i], arr[j]) > 0 {
        let tmp = arr[i]
        arr[i] = arr[j]
        arr[j] = tmp
      }
    }
  }
  let out = []
  for i = 0; i < arr.length(); i = i + 1 {
    if i == 0 || arr[i] != arr[i - 1] {
      out.push(arr[i])
    }
  }
  out
}

///|
pub fn parse(answer : String) -> Result[Guess, IdentifyError] {
  let root = match extract_json(answer) {
    Some(v) => v
    None => return Err(Unparseable("no JSON object found"))
  }
  let obj = match root {
    Object(m) => m
    _ => return Err(Unparseable("expected JSON object"))
  }

  // Refusal cases
  match obj.get("no_card") {
    Some(True) => {
      let reason = match obj.get("reason") {
        Some(String(r)) => r
        _ => "model declined: no card visible"
      }
      return Err(NoCard(reason))
    }
    _ => ()
  }
  match obj.get("cards_visible") {
    Some(Number(n, ..)) => {
      if n.to_int() > 1 {
        return Err(MoreThanOneCard)
      }
    }
    _ => ()
  }

  // Name is required
  let name = match str_field(obj, "name") {
    Some(n) => n
    None => return Err(NoName)
  }

  let set_name = match str_field(obj, "set_name") {
    Some(s) => s
    None => ""
  }
  let set_code = match str_field(obj, "set_code") {
    Some(s) => s.to_lower()
    None => ""
  }
  let (number, number_flag) = normalize_number(str_field(obj, "number"))
  let rarity = match str_field(obj, "rarity") {
    Some(s) => s
    None => ""
  }
  let language = match str_field(obj, "language") {
    Some(s) => s.to_lower()
    None => ""
  }

  // Confidence
  let confidence : Byte = match obj.get("confidence") {
    None => b'\x00'
    Some(Number(n, ..)) => {
      let val = n.to_int()
      if val < 0 || val > 100 {
        return Err(Refused("confidence outside 0..=100"))
      }
      val.to_byte()
    }
    Some(_) => return Err(Refused("non-numeric confidence"))
  }

  let (variant, variant_flag) = match str_field(obj, "variant") {
    None => (None, true)
    Some(s) => match parse_variant(s) {
      Some(v) => (Some(v), false)
      None => (None, true)
    }
  }

  let graded = match obj.get("graded") {
    Some(v) => parse_graded(v)
    None => None
  }

  let (condition, condition_flag) = match graded {
    Some(_) => (None, false)
    None => match str_field(obj, "condition") {
      None => (None, true)
      Some(s) => match parse_condition(s) {
        Some(c) => (Some(c), false)
        None => (None, true)
      }
    }
  }

  let needs_review = []
  if number_flag {
    needs_review.push("number")
  }
  if rarity.length() == 0 {
    needs_review.push("rarity")
  }
  if language.length() == 0 {
    needs_review.push("language")
  }
  if variant_flag {
    needs_review.push("variant")
  }
  if condition_flag {
    needs_review.push("condition")
  }

  match obj.get("uncertain") {
    Some(Array(arr)) => {
      for item in arr {
        match item {
          String(s) => needs_review.push(s)
          _ => ()
        }
      }
    }
    _ => ()
  }

  let deduped_review = sort_and_dedup(needs_review)

  Ok({
    name,
    set_name,
    set_code,
    number,
    rarity,
    language,
    printing: variant,
    condition,
    graded,
    confidence,
    needs_review: deduped_review,
  })
}

///|
pub fn prompt() -> String {
  "Look at this photo of a trading card and answer with a single JSON object, nothing else.\n\nIf there is no card in the photo, reply with {\"no_card\": true, \"reason\": \"...\"}.\nIf more than one card is visible, reply with {\"cards_visible\": <count>}.\n\nOtherwise reply with these fields, using an empty string or omitting a field you cannot establish from the photo (never guess or default one):\n{\n  \"name\": \"the card's name\",\n  \"set_name\": \"the set name as printed\",\n  \"set_code\": \"the set's lookup code\",\n  \"number\": \"the card number as printed, e.g. 58/165 or #58\",\n  \"rarity\": \"the rarity as printed\",\n  \"language\": \"the ISO-639-1 language code\",\n  \"variant\": \"normal | holo | reverse holo | 1st edition | shadowless | full art | alt art | secret rare\",\n  \"condition\": \"mint | near mint | lightly played | moderately played | heavily played | damaged\",\n  \"graded\": \"e.g. PSA 10, or omit if not slabbed\",\n  \"confidence\": 0-100,\n  \"uncertain\": [\"field names you are not sure about\"]\n}"
}
