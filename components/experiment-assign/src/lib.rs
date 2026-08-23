//! `experiment-assign` — split subjects across named A/B/n variants by weight, stickily — a subject stays in its arm
//!
//! Weighted, sticky, named-variant assignment backed by `wasi:keyvalue`.
//!
//! A variant set lives in kv under `ex_{tenant}/{name}`, serialized as
//! `variant:weight,variant:weight,…` (variant names sanitized). `""` tenant is
//! the global key. `assign` resolves the tenant set first, then the global set.
//!
//! Bucketing: normalize weights onto a 0..=999 cumulative range, then
//! `hash(subject) % 1000` picks the arm whose cumulative slice contains it.
//! Because slices are laid out in a fixed order and only GROW when a weight
//! rises, a subject already inside an arm stays there — assignment is sticky and
//! weight changes are monotone per arm. Same FNV-1a hash as `featureflags:guard`.

#[allow(warnings)]
mod bindings;

use bindings::exports::experiment::assign::assigner::{
    Arm, AssignError, Assignment, Context, Guest,
};
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
const PREFIX: &str = "ex_";
const RANGE: u64 = 1000; // cumulative-weight resolution (0.1% granularity)
const COHORT_MAX: u32 = 1000;

// ---- key scheme (mirrors feature-flags) ---------------------------------

fn exp_key(tenant: &str, name: &str) -> String {
    let mut out = String::with_capacity(name.len() + tenant.len() + 4);
    out.push_str(PREFIX);
    sanitize_into(&mut out, tenant);
    out.push('/');
    sanitize_into(&mut out, name);
    out
}

fn sanitize_into(out: &mut String, s: &str) {
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
}

fn open() -> Result<kv::Bucket, AssignError> {
    kv::open(BUCKET).map_err(|e| AssignError::BackendUnavailable(format!("open: {e:?}")))
}

// ---- variant-set (de)serialization ---------------------------------------

/// `name:weight` pairs, comma-joined. Arm names are sanitized so `,` / `:`
/// can't appear literally; weights are plain decimal.
fn variants_to_bytes(variants: &[Arm]) -> Vec<u8> {
    let mut parts: Vec<String> = Vec::with_capacity(variants.len());
    for v in variants {
        let mut enc = String::new();
        sanitize_into(&mut enc, &v.name);
        parts.push(format!("{}:{}", enc, v.weight));
    }
    parts.join(",").into_bytes()
}

fn variants_from_bytes(bytes: &[u8]) -> Option<Vec<Arm>> {
    let s = std::str::from_utf8(bytes).ok()?;
    if s.is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for part in s.split(',') {
        let (enc, w) = part.rsplit_once(':')?;
        out.push(Arm { name: unsanitize(enc), weight: w.parse().ok()? });
    }
    Some(out)
}

fn unsanitize(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'_' && i + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[i + 1..i + 3]) {
                if let Ok(b) = u8::from_str_radix(hex, 16) {
                    out.push(b);
                    i += 3;
                    continue;
                }
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---- bucketing ------------------------------------------------------------

/// FNV-1a 64-bit — deterministic, stable across runs (same as featureflags).
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Pick a variant name for `subject` over `variants`. Lays each variant on a
/// cumulative slice of 0..RANGE proportional to its weight, then indexes by
/// `hash(subject) % RANGE`. Fixed variant order → slices only grow when a
/// weight rises → sticky. Returns None if total weight is zero.
fn pick<'a>(variants: &'a [Arm], subject: &str) -> Option<&'a str> {
    let total: u64 = variants.iter().map(|v| v.weight as u64).sum();
    if total == 0 {
        return None;
    }
    let point = fnv1a(subject) % RANGE;
    // Scale the point into weight-space so we compare against cumulative raw
    // weights directly (avoids rounding drift from per-variant normalization).
    let scaled = point * total / RANGE;
    let mut acc: u64 = 0;
    for v in variants {
        acc += v.weight as u64;
        if scaled < acc {
            return Some(&v.name);
        }
    }
    // Floating edge (scaled == total-1 with integer division): last positive arm.
    variants.iter().rev().find(|v| v.weight > 0).map(|v| v.name.as_str())
}

// ---- read helpers ---------------------------------------------------------

fn read_set(bucket: &kv::Bucket, key: &str) -> Result<Option<Vec<Arm>>, AssignError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => Ok(variants_from_bytes(&bytes)),
        Ok(None) => Ok(None),
        Err(e) => Err(AssignError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

/// Effective variant set: tenant definition if present, else global.
fn effective(bucket: &kv::Bucket, name: &str, tenant: &str) -> Result<Vec<Arm>, AssignError> {
    if !tenant.is_empty() {
        if let Some(vs) = read_set(bucket, &exp_key(tenant, name))? {
            return Ok(vs);
        }
    }
    match read_set(bucket, &exp_key("", name))? {
        Some(vs) => Ok(vs),
        None => Err(AssignError::NotFound),
    }
}

impl Guest for Component {
    fn set_experiment(name: String, tenant: String, variants: Vec<Arm>) -> Result<(), AssignError> {
        if variants.is_empty() {
            return Err(AssignError::InvalidVariants("no variants".into()));
        }
        if variants.iter().all(|v| v.weight == 0) {
            return Err(AssignError::InvalidVariants("all weights zero".into()));
        }
        let bucket = open()?;
        bucket
            .set(&exp_key(&tenant, &name), &variants_to_bytes(&variants))
            .map_err(|e| AssignError::BackendUnavailable(format!("set: {e:?}")))
    }

    fn clear_experiment(name: String, tenant: String) -> Result<(), AssignError> {
        let bucket = open()?;
        bucket
            .delete(&exp_key(&tenant, &name))
            .map_err(|e| AssignError::BackendUnavailable(format!("delete: {e:?}")))
    }

    fn assign(name: String, ctx: Context) -> Result<String, AssignError> {
        let bucket = open()?;
        let variants = effective(&bucket, &name, &ctx.tenant)?;
        pick(&variants, &ctx.subject)
            .map(|s| s.to_string())
            .ok_or_else(|| AssignError::InvalidVariants("total weight zero".into()))
    }

    fn describe(name: String, tenant: String) -> Result<Vec<Arm>, AssignError> {
        let bucket = open()?;
        effective(&bucket, &name, &tenant)
    }

    fn cohort(name: String, tenant: String, n: u32) -> Result<Vec<Assignment>, AssignError> {
        let bucket = open()?;
        let variants = effective(&bucket, &name, &tenant)?;
        let n = n.clamp(1, COHORT_MAX);
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            let subject = format!("subject-{i}");
            let arm = pick(&variants, &subject)
                .ok_or_else(|| AssignError::InvalidVariants("total weight zero".into()))?
                .to_string();
            out.push(Assignment { subject, arm });
        }
        Ok(out)
    }
}

bindings::export!(Component with_types_in bindings);
