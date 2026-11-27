///|
test "routine visit uses standard rate" {
  let d = @evaluator.evaluate({
    species: "dog",
    age_years: 4U,
    minutes: 30U,
    symptoms: "annual checkup",
    owner_tier: "standard",
    tenant: "north-clinic",
  })
  assert_eq(d.priority, "routine")
  assert_eq(d.fee_cents, 7500UL)
  assert_eq(d.variant_id, "triage-v1-mbt")
}

///|
test "emergency symptoms escalate immediately" {
  let d = @evaluator.evaluate({
    species: "cat",
    age_years: 11U,
    minutes: 15U,
    symptoms: "sudden seizure",
    owner_tier: "vip",
    tenant: "north-clinic",
  })
  assert_eq(d.priority, "emergency")
  assert_eq(d.recommended_vet, "er-oncall")
}
