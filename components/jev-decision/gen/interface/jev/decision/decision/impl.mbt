// Reference provider for jev:decision@0.1.0.

const MATCH_CONFIDENCE : UInt = 800U
const NO_MATCH_CONFIDENCE : UInt = 100U
const NEUTRAL_PROBABILITY : UInt = 500U

fn str_contains(s : String, needle : String) -> Bool {
  let s_len = s.length()
  let n_len = needle.length()
  if n_len == 0 {
    return true
  }
  if n_len > s_len {
    return false
  }
  for i = 0; i <= s_len - n_len; i = i + 1 {
    let mut matched = true
    for j = 0; j < n_len; j = j + 1 {
      if s[i + j] != needle[j] {
        matched = false
        break
      }
    }
    if matched {
      return true
    }
  }
  false
}

///|
pub fn choose(req : ChoiceRequest) -> Result[ChoiceResult, DecisionError] {
  if req.options.length() == 0 {
    return Err(InvalidRequest("no options offered"))
  }
  let haystack = req.state.to_lower()
  let mut hit : Int? = None
  for i = 0; i < req.options.length(); i = i + 1 {
    if str_contains(haystack, req.options[i].to_lower()) {
      hit = Some(i)
      break
    }
  }

  let (selected, confidence, flat) = match hit {
    Some(i) => (req.options[i], MATCH_CONFIDENCE, false)
    None => (req.options[0], NO_MATCH_CONFIDENCE, true)
  }

  let remainder : UInt = if req.options.length() > 1 {
    (1000U - confidence) / (req.options.length() - 1).reinterpret_as_uint()
  } else {
    0U
  }

  let distribution : Array[OptionScore] = []
  for o in req.options {
    let score = if o == selected { confidence } else { remainder }
    distribution.push({ label: o, score })
  }

  Ok({ selected, confidence, distribution, flat })
}

///|
pub fn score(req : ScoreRequest) -> Result[ScoreResult, DecisionError] {
  if req.levels.length() < 2 {
    return Err(InvalidRequest("at least two levels are required"))
  }
  let mid = Float::from_int64((req.levels.length() - 1).to_int64()) / (2.0 : Float)
  let each : UInt = 1000U / req.levels.length().reinterpret_as_uint()
  let distribution : Array[OptionScore] = []
  for l in req.levels {
    distribution.push({ label: l, score: each })
  }
  Ok({ value: mid, confidence: NO_MATCH_CONFIDENCE, distribution })
}

///|
pub fn gate(req : GateRequest) -> Result[GateResult, DecisionError] {
  if req.instructions.length() == 0 {
    return Err(InvalidRequest("no instructions given"))
  }
  Ok({ probability: NEUTRAL_PROBABILITY })
}

///|
pub fn evaluate(
  state : String,
  questions : Array[Question],
) -> Result[Array[Answered], DecisionError] {
  if questions.length() == 0 {
    return Err(InvalidRequest("no questions given"))
  }
  let out : Array[Answered] = []
  for q in questions {
    let outcome = match q.kind {
      Choice(options) =>
        match choose({ state, instructions: q.instructions, options }) {
          Ok(c) => Ok(AnswerKind::Choice(c))
          Err(e) => Err(e)
        }
      Score(levels) =>
        match score({ state, instructions: q.instructions, levels }) {
          Ok(s) => Ok(AnswerKind::Score(s))
          Err(e) => Err(e)
        }
      Gate(_) =>
        match
          gate({
            state,
            instructions: q.instructions,
            true_hint: "",
            false_hint: "",
          }) {
          Ok(g) => Ok(AnswerKind::Gate(g))
          Err(e) => Err(e)
        }
    }
    out.push({ id: q.id, outcome })
  }
  Ok(out)
}

///|
pub fn describe() -> (String, Bool) {
  ("jev-decision-reference", true)
}
