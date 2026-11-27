#!/usr/bin/env bash
# On-demand MoonBit component builder for pet-clinic triage variants.
#
# Compiles a tiny wasm32-wasip2 component implementing `clinic:triage@0.1.0`
# in <500ms and prints its path, size, and sha256 digest for immediate
# registration in the delivery SDLC controller.
#
# Usage:
#   tools/moonbit-variant.sh build-baseline
#   tools/moonbit-variant.sh create <variant-id> [--vip-discount-pct N] [--senior-age N] [--avian-specialist NAME]
set -euo pipefail
cd "$(dirname "$0")/.." || exit 1
root="$PWD"
moon="${MOON:-$HOME/.moon/bin/moon}"
wasm_tools="${WASM_TOOLS:-$HOME/.cargo/bin/wasm-tools}"
out_dir="$root/components/target/wasm32-wasip2/release"
mkdir -p "$out_dir"

build_component() {
  local src_dir="$1"
  local out_wasm="$2"
  local t0 t1 ms size_kb digest
  t0=$(date +%s%3N)
  (cd "$src_dir" && "$moon" build --target wasm --release --quiet)
  local core="$src_dir/_build/wasm/release/build/gen/gen.wasm"
  local embedded
  embedded="$(mktemp "${TMPDIR:-/tmp}/mbt_embed.XXXXXX").wasm"
  trap 'rm -f "$embedded"' RETURN
  "$wasm_tools" component embed "$src_dir/wit" "$core" \
    --world clinic-triage --encoding utf16 -o "$embedded"
  "$wasm_tools" component new "$embedded" -o "$out_wasm"
  t1=$(date +%s%3N)
  ms=$((t1 - t0))
  size_kb=$(( $(wc -c < "$out_wasm") / 1024 ))
  digest=$(sha256sum "$out_wasm" | awk '{print $1}')
  printf '{"path":"%s","size_kb":%d,"build_ms":%d,"digest":"sha256:%s"}\n' \
    "${out_wasm#"$root"/}" "$size_kb" "$ms" "$digest"
}

cmd="${1:-}"
case "$cmd" in
build-baseline)
  build_component "$root/components/clinic-triage" "$out_dir/clinic_triage.wasm"
  ;;
create)
  variant_id="${2:?usage: moonbit-variant.sh create <variant-id> [--vip-discount-pct N] [--senior-age N] [--avian-specialist NAME]}"
  shift 2
  vip_discount=20
  senior_age=7
  avian_vet="dr-avian-specialist"
  while [ $# -gt 0 ]; do
    case "$1" in
      --vip-discount-pct) vip_discount="$2"; shift 2 ;;
      --senior-age) senior_age="$2"; shift 2 ;;
      --avian-specialist) avian_vet="$2"; shift 2 ;;
      *) echo "unknown flag: $1" >&2; exit 1 ;;
    esac
  done

  work_dir="$root/components/target/moonbit-variants/$variant_id"
  rm -rf "$work_dir"
  mkdir -p "$work_dir"
  cp -r "$root/components/clinic-triage/moon.mod" \
        "$root/components/clinic-triage/wit" \
        "$root/components/clinic-triage/gen" \
        "$root/components/clinic-triage/world" "$work_dir/"

  cat > "$work_dir/gen/interface/clinic/triage/evaluator/impl.mbt" <<EOF
///|
fn contains_ci(haystack : String, needle : String) -> Bool {
  haystack.to_lower().contains(needle.to_lower())
}

///|
/// On-demand MoonBit variant ($variant_id): broadened clinical symptoms,
/// senior/avian emergency co-alerts, ER slot guard, and VIP discount.
pub fn evaluate(ctx : VisitContext) -> TriageDecision {
  let base_fee : UInt64 = match ctx.minutes {
    15U => 4500UL
    30U => 7500UL
    60U => 12500UL
    _ => 7500UL
  }
  let fee_cents = if ctx.owner_tier == "vip" {
    base_fee * (100UL - ${vip_discount}UL) / 100UL
  } else {
    base_fee
  }
  let is_senior = ctx.age_years >= ${senior_age}U
  let is_bird = ctx.species == "bird"
  let is_emergency = contains_ci(ctx.symptoms, "bleeding") ||
    contains_ci(ctx.symptoms, "seizure") ||
    contains_ci(ctx.symptoms, "convulsion") ||
    contains_ci(ctx.symptoms, "collapse") ||
    contains_ci(ctx.symptoms, "unresponsive") ||
    contains_ci(ctx.symptoms, "difficulty breathing")
  let is_urgent = contains_ci(ctx.symptoms, "fever") ||
    contains_ci(ctx.symptoms, "vomiting") ||
    contains_ci(ctx.symptoms, "limping") ||
    contains_ci(ctx.symptoms, "diarrhea") ||
    contains_ci(ctx.symptoms, "letharg")
  let (priority, vet, rec) = if is_emergency {
    let co_alert = if is_senior && is_bird {
      " Co-alert geriatric & ${avian_vet}; order renal/metabolic panel."
    } else if is_senior {
      " Geriatric caution: order renal/metabolic panel on intake."
    } else if is_bird {
      " Co-alert ${avian_vet} for avian stabilization."
    } else {
      ""
    }
    let slot_note = if ctx.minutes < 30U {
      " Expand slot to >=30m ER block."
    } else {
      ""
    }
    (
      "emergency",
      "er-oncall",
      "Immediate stabilization required; pre-alert ER team." + co_alert + slot_note,
    )
  } else if is_urgent {
    (
      "urgent",
      if is_bird { "${avian_vet}" } else { "dr-urgent-care" },
      "Same-day urgent slot with pre-visit diagnostics.",
    )
  } else if is_senior {
    (
      "priority",
      if is_bird { "${avian_vet}" } else { "dr-geriatric" },
      "Senior pet fast-track: include blood panel & mobility check.",
    )
  } else if is_bird {
    (
      "routine",
      "${avian_vet}",
      "Avian specialist wellness exam.",
    )
  } else {
    (
      "routine",
      "dr-general",
      "Standard wellness visit with digital follow-up care plan.",
    )
  }
  {
    priority,
    fee_cents,
    recommended_vet: vet,
    recommendation: rec,
    variant_id: "${variant_id}",
  }
}

///|
pub fn variant_info() -> String {
  "${variant_id}: on-demand MoonBit triage v3 (senior>=${senior_age}, vip=-${vip_discount}%, avian=${avian_vet}, broadened symptoms + ER co-alerts)"
}
EOF

  snake="${variant_id//-/_}"
  build_component "$work_dir" "$out_dir/clinic_triage_${snake}.wasm"
  ;;
*)
  echo "usage: $0 <build-baseline | create <variant-id> [...]>" >&2
  exit 1
  ;;
esac
