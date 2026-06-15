//! `feature-flags` — reference implementation of `featureflags:guard`.
//!
//! Flag evaluation + runtime management backed by `wasi:keyvalue` (runtime
//! rules) + `wasi:config` (deploy-time definitions).
//!
//! Runtime rules live in kv under `ff_{tenant}_{flag}` (tenant `""` = global),
//! serialized as one of: `e` (enabled), `d` (disabled), `p{N}` (percentage).
//!
//! Resolution order for `is-enabled(flag, ctx)`:
//!   1. tenant rule `ff_{tenant}_{flag}`
//!   2. global rule  `ff__{flag}`
//!   3. config `flag:{flag}`   (bool or `N%`)
//!   4. `false`
//!
//! Percentage rollouts bucket on a stable FNV-1a hash of `ctx.subject`, so the
//! same subject is sticky across calls (no flicker).

#[allow(warnings)]
mod bindings;

use bindings::exports::featureflags::guard::evaluator::{
    Context, FlagError, FlagState, Guest, Rule, Source,
};
use bindings::wasi::config::runtime as config;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";
const PREFIX: &str = "ff_";

// ---- key scheme ---------------------------------------------------------

/// Runtime-rule kv key: `ff_{tenant}/{flag}`, both parts sanitized so neither
/// the separator `/` nor the escape char `_` can appear inside a part. A `""`
/// tenant yields the global key `ff_/{flag}`.
fn rule_key(tenant: &str, flag: &str) -> String {
    let mut out = String::with_capacity(flag.len() + tenant.len() + 4);
    out.push_str(PREFIX);
    sanitize_into(&mut out, tenant);
    out.push('/');
    sanitize_into(&mut out, flag);
    out
}

fn sanitize_into(out: &mut String, s: &str) {
    for b in s.bytes() {
        match b {
            // '/' (separator) and '_' (escape lead) are deliberately escaped so
            // they cannot appear literally inside a sanitized part.
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
}

/// Inverse of `sanitize_into`: decode `_HH` escapes back to the original name
/// (for display in `list`). Invalid escapes are passed through best-effort.
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

fn open() -> Result<kv::Bucket, FlagError> {
    kv::open(BUCKET).map_err(|e| FlagError::BackendUnavailable(format!("open: {e:?}")))
}

// ---- rule (de)serialization ---------------------------------------------

fn rule_to_bytes(rule: &Rule) -> Vec<u8> {
    match rule {
        Rule::Enabled => b"e".to_vec(),
        Rule::Disabled => b"d".to_vec(),
        Rule::Percentage(n) => format!("p{}", (*n).min(100)).into_bytes(),
    }
}

fn rule_from_bytes(bytes: &[u8]) -> Option<Rule> {
    match bytes.first()? {
        b'e' => Some(Rule::Enabled),
        b'd' => Some(Rule::Disabled),
        b'p' => {
            let n: u8 = std::str::from_utf8(&bytes[1..]).ok()?.parse().ok()?;
            Some(Rule::Percentage(n.min(100)))
        }
        _ => None,
    }
}

/// Parse a config value (`true`/`on`/`1`/`yes`, `false`/...`, or `N%`) into a rule.
fn rule_from_config(val: &str) -> Option<Rule> {
    let v = val.trim();
    if let Some(pct) = v.strip_suffix('%') {
        let n: u32 = pct.trim().parse().ok()?;
        return Some(Rule::Percentage(n.min(100) as u8));
    }
    match v.to_ascii_lowercase().as_str() {
        "true" | "on" | "1" | "yes" | "enabled" => Some(Rule::Enabled),
        "false" | "off" | "0" | "no" | "disabled" => Some(Rule::Disabled),
        _ => None,
    }
}

// ---- evaluation ---------------------------------------------------------

/// FNV-1a 64-bit hash — small, deterministic, dependency-free. Stable across
/// runs so a subject's rollout bucket never changes.
fn fnv1a(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn rule_verdict(rule: &Rule, subject: &str) -> bool {
    match rule {
        Rule::Enabled => true,
        Rule::Disabled => false,
        Rule::Percentage(0) => false,
        Rule::Percentage(n) if *n >= 100 => true,
        Rule::Percentage(n) => fnv1a(subject) % 100 < *n as u64,
    }
}

/// Read a runtime rule for (tenant, flag) from kv, if set.
fn kv_rule(bucket: &kv::Bucket, tenant: &str, flag: &str) -> Result<Option<Rule>, FlagError> {
    kv_rule_by_key(bucket, &rule_key(tenant, flag))
}

/// Read a runtime rule by its already-formed kv key.
fn kv_rule_by_key(bucket: &kv::Bucket, key: &str) -> Result<Option<Rule>, FlagError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => Ok(rule_from_bytes(&bytes)),
        Ok(None) => Ok(None),
        Err(e) => Err(FlagError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

impl Guest for Component {
    fn is_enabled(flag: String, ctx: Context) -> Result<bool, FlagError> {
        let bucket = open()?;
        // 1. tenant-scoped runtime rule.
        if !ctx.tenant.is_empty() {
            if let Some(r) = kv_rule(&bucket, &ctx.tenant, &flag)? {
                return Ok(rule_verdict(&r, &ctx.subject));
            }
        }
        // 2. global runtime rule.
        if let Some(r) = kv_rule(&bucket, "", &flag)? {
            return Ok(rule_verdict(&r, &ctx.subject));
        }
        // 3. config definition.
        if let Ok(Some(v)) = config::get(&format!("flag:{flag}")) {
            if let Some(r) = rule_from_config(&v) {
                return Ok(rule_verdict(&r, &ctx.subject));
            }
        }
        // 4. unknown flag -> off.
        Ok(false)
    }

    fn set_rule(flag: String, tenant: String, rule: Rule) -> Result<(), FlagError> {
        let bucket = open()?;
        bucket
            .set(&rule_key(&tenant, &flag), &rule_to_bytes(&rule))
            .map_err(|e| FlagError::BackendUnavailable(format!("set: {e:?}")))
    }

    fn clear_rule(flag: String, tenant: String) -> Result<(), FlagError> {
        let bucket = open()?;
        bucket
            .delete(&rule_key(&tenant, &flag))
            .map_err(|e| FlagError::BackendUnavailable(format!("delete: {e:?}")))
    }

    fn list_flags(tenant: String) -> Result<Vec<FlagState>, FlagError> {
        // Merge config definitions + global rules + tenant rules. Later sources
        // shadow earlier ones: config < global-override < tenant-override.
        // Keyed by flag name, preserving insertion of the winning rule+source.
        let mut names: Vec<String> = Vec::new();
        let mut chosen: Vec<(Rule, Source)> = Vec::new();

        // Priority so the merge is order-independent: config < global < tenant.
        let rank = |s: Source| match s {
            Source::Config => 0u8,
            Source::GlobalOverride => 1,
            Source::TenantOverride => 2,
        };
        let mut upsert = |name: &str, rule: Rule, source: Source| {
            if let Some(i) = names.iter().position(|n| n == name) {
                if rank(source) >= rank(chosen[i].1) {
                    chosen[i] = (rule, source);
                }
            } else {
                names.push(name.to_string());
                chosen.push((rule, source));
            }
        };

        // config: `flag:{name}` -> rule.
        if let Ok(pairs) = config::get_all() {
            for (k, v) in pairs {
                if let Some(name) = k.strip_prefix("flag:") {
                    if let Some(r) = rule_from_config(&v) {
                        upsert(name, r, Source::Config);
                    }
                }
            }
        }

        // runtime rules from kv. global keys are `ff_/{flag}`; tenant keys are
        // `ff_{tenant}/{flag}`. The `{flag}` part is sanitized — decode it back
        // for display, but read the value by the full key (no re-sanitizing).
        let bucket = open()?;
        let global_prefix = format!("{PREFIX}/"); // ff_/ (empty tenant)
        let tenant_prefix = if tenant.is_empty() {
            String::new()
        } else {
            let mut p = String::from(PREFIX);
            sanitize_into(&mut p, &tenant);
            p.push('/');
            p // ff_{tenant}/
        };

        let mut cursor: Option<u64> = None;
        loop {
            let page = bucket
                .list_keys(cursor)
                .map_err(|e| FlagError::BackendUnavailable(format!("list-keys: {e:?}")))?;
            for key in &page.keys {
                // tenant rule wins, so check it first (only the queried tenant).
                if !tenant_prefix.is_empty() {
                    if let Some(enc) = key.strip_prefix(&tenant_prefix) {
                        if let Some(r) = kv_rule_by_key(&bucket, key)? {
                            upsert(&unsanitize(enc), r, Source::TenantOverride);
                        }
                        continue;
                    }
                }
                // global rule: ff_/{flag}
                if let Some(enc) = key.strip_prefix(&global_prefix) {
                    if let Some(r) = kv_rule_by_key(&bucket, key)? {
                        upsert(&unsanitize(enc), r, Source::GlobalOverride);
                    }
                }
            }
            match page.cursor {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }

        Ok(names
            .into_iter()
            .zip(chosen)
            .map(|(name, (rule, source))| FlagState { name, rule, source })
            .collect())
    }
}

bindings::export!(Component with_types_in bindings);
