// Tests for jev-decision.

test "a substring match wins with high confidence" {
  let req : ChoiceRequest = {
    state: "the printer is jammed again",
    instructions: "classify",
    options: ["helpdesk-domain", "clinic-domain"],
  }
  let out = choose(req)
  match out {
    Ok(res) => {
      assert_eq(res.selected, "helpdesk-domain")
      assert_eq(res.flat, true)
    }
    Err(_) => panic()
  }
}

test "a literal option in the state is selected confidently" {
  let req : ChoiceRequest = {
    state: "please route this to clinic-domain",
    instructions: "classify",
    options: ["helpdesk-domain", "clinic-domain"],
  }
  let out = choose(req)
  match out {
    Ok(res) => {
      assert_eq(res.selected, "clinic-domain")
      assert_eq(res.confidence, 800U)
      assert_eq(res.flat, false)
    }
    Err(_) => panic()
  }
}

test "empty options are rejected" {
  let req : ChoiceRequest = {
    state: "anything",
    instructions: "classify",
    options: [],
  }
  match choose(req) {
    Err(InvalidRequest(_)) => ()
    _ => panic()
  }
}

test "distribution sums close to 1000" {
  let req : ChoiceRequest = {
    state: "no match here",
    instructions: "classify",
    options: ["a", "b", "c"],
  }
  let out = choose(req)
  match out {
    Ok(res) => {
      let mut total : UInt = 0U
      for d in res.distribution {
        total = total + d.score
      }
      assert_eq(total <= 1000U, true)
    }
    Err(_) => panic()
  }
}

test "fewer than two levels is rejected" {
  let req : ScoreRequest = {
    state: "x",
    instructions: "rate",
    levels: ["only"],
  }
  match score(req) {
    Err(InvalidRequest(_)) => ()
    _ => panic()
  }
}

test "score reports midpoint of rubric" {
  let req : ScoreRequest = {
    state: "x",
    instructions: "rate",
    levels: ["low", "medium", "high"],
  }
  let out = score(req)
  match out {
    Ok(res) => {
      assert_eq(res.value, (1.0 : Float))
      assert_eq(res.distribution.length(), 3)
    }
    Err(_) => panic()
  }
}

test "gate reports neutral probability" {
  let req : GateRequest = {
    state: "x",
    instructions: "is it ok?",
    true_hint: "",
    false_hint: "",
  }
  let out = gate(req)
  match out {
    Ok(res) => assert_eq(res.probability, 500U)
    Err(_) => panic()
  }
}

test "evaluate answers every question by its own id" {
  let questions : Array[Question] = [
    {
      id: "a",
      instructions: "classify",
      kind: Choice(["helpdesk-domain", "clinic-domain"]),
    },
    {
      id: "b",
      instructions: "is this urgent?",
      kind: Gate({ true_hint: "", false_hint: "" }),
    },
  ]
  let out = evaluate("route to clinic-domain", questions)
  match out {
    Ok(res) => {
      assert_eq(res.length(), 2)
      assert_eq(res[0].id, "a")
      assert_eq(res[1].id, "b")
      match res[0].outcome {
        Ok(Choice(c)) => assert_eq(c.selected, "clinic-domain")
        _ => panic()
      }
      match res[1].outcome {
        Ok(Gate(g)) => assert_eq(g.probability, 500U)
        _ => panic()
      }
    }
    Err(_) => panic()
  }
}

test "evaluate rejects empty batch" {
  match evaluate("anything", []) {
    Err(InvalidRequest(_)) => ()
    _ => panic()
  }
}

test "describe returns reference name" {
  let (name, score_impl) = describe()
  assert_eq(name, "jev-decision-reference")
  assert_eq(score_impl, true)
}
