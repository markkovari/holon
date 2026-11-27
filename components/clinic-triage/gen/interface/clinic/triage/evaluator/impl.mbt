///|
fn contains_ci(haystack : String, needle : String) -> Bool {
  haystack.to_lower().contains(needle.to_lower())
}

///|
/// Control variant (v1): standard flat-rate pet-clinic triage and pricing.
pub fn evaluate(ctx : VisitContext) -> TriageDecision {
  let base_fee : UInt64 = match ctx.minutes {
    15U => 4500UL
    30U => 7500UL
    60U => 12500UL
    _ => 7500UL
  }
  let (priority, vet, rec) = if contains_ci(ctx.symptoms, "bleeding") ||
    contains_ci(ctx.symptoms, "seizure") ||
    contains_ci(ctx.symptoms, "collapse") {
    (
      "emergency",
      "er-oncall",
      "Immediate stabilization required; bypass standard queue.",
    )
  } else if contains_ci(ctx.symptoms, "fever") ||
    contains_ci(ctx.symptoms, "vomiting") ||
    contains_ci(ctx.symptoms, "limping") {
    (
      "urgent",
      "dr-general",
      "Schedule same-day assessment and vitals check.",
    )
  } else {
    (
      "routine",
      "dr-general",
      "Standard wellness visit and preventive checkup.",
    )
  }
  {
    priority,
    fee_cents: base_fee,
    recommended_vet: vet,
    recommendation: rec,
    variant_id: "triage-v1-mbt",
  }
}

///|
pub fn variant_info() -> String {
  "triage-v1-mbt: baseline flat-rate pet-clinic triage (MoonBit)"
}
