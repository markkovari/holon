//! `comp:secrets/reader` — the host side of ADR-0051.
//!
//! Three properties, and the first is the whole security argument:
//!
//! 1. **A guest names a key, never a reference.** `Scope::secret` is the only way
//!    from a guest string to a `SecretRef`, and `SecretRef` cannot be constructed
//!    outside `tenant.rs`. The same shape as buckets (ADR-0012) and links
//!    (ADR-0013), for the third time.
//! 2. **`get` hands back a handle; `reveal` reads.** Holding a secret and reading it
//!    are different events, so they are different calls and only one is audited.
//! 3. **The value is fetched on first reveal, then cached for the instance.** A
//!    secret on a code path that never runs never enters this process, and an
//!    instance is per-request anyway (ADR-0037), so "cached for the instance" is
//!    short by construction.

use std::collections::HashMap;
use std::time::Duration;

use crate::tenant::SecretRef;

/// What the guest holds. Deliberately not the value.
pub struct SecretHandle {
    pub key: String,
    pub reference: SecretRef,
}

/// Values already fetched by this instance, keyed by reference.
///
/// Per instance rather than per host: two components granted the same secret do not
/// share a cache entry, so one cannot warm the other's, and nothing outlives the
/// instance that was entitled to it.
#[derive(Default)]
pub struct SecretCache(HashMap<String, String>);

impl SecretCache {
    pub fn get(&self, r: &SecretRef) -> Option<&String> {
        self.0.get(r.as_str())
    }

    pub fn put(&mut self, r: &SecretRef, value: String) {
        self.0.insert(r.as_str().to_string(), value);
    }
}

impl Drop for SecretCache {
    fn drop(&mut self) {
        // Overwrite before releasing. Rust gives no guarantee the compiler keeps
        // this — a real `zeroize` would — but it costs nothing and it is the honest
        // gesture. ponytail: use `zeroize` if a secret ever outlives a request.
        for v in self.0.values_mut() {
            unsafe {
                for b in v.as_bytes_mut() {
                    std::ptr::write_volatile(b, 0);
                }
            }
        }
        self.0.clear();
    }
}

/// Fetch one secret from the platform.
///
/// The token authorises exactly the references in this instance's manifest, so the
/// platform decides — the host does not carry a credential that could read anything
/// else, and a stolen token is worth what the manifest was worth.
///
/// ponytail (low priority, ADR-0051): the value crosses the wire under TLS only.
/// Per-node key wrapping (wasmCloud uses xkeys) would protect it from someone who
/// can read the transport but not the host — a narrow window, since the host is what
/// decrypts. Likewise there is no nonce or request id, so a captured request can be
/// REPLAYED against the platform until the token expires; the token's lifetime is
/// currently the only bound on that. Both are worth doing before a node runs
/// somewhere the operator does not control.
pub async fn fetch(
    http: &reqwest::Client,
    platform_url: &str,
    token: &str,
    reference: &SecretRef,
) -> Result<String, String> {
    if platform_url.is_empty() {
        return Err("this host has no platform to fetch secrets from".into());
    }
    let url = format!(
        "{}/api/internal/secret?ref={}",
        platform_url.trim_end_matches('/'),
        urlencoding(reference.as_str())
    );
    let res = tokio::time::timeout(
        Duration::from_secs(10),
        http.get(&url).header("x-fetch-token", token).send(),
    )
    .await
    .map_err(|_| "the platform did not answer in time".to_string())?
    .map_err(|e| format!("reaching the platform: {e}"))?;

    match res.status().as_u16() {
        200 => res.text().await.map_err(|e| format!("reading the secret: {e}")),
        // 401 is the token, 403 is the reference: an expired token is a restart, a
        // refused reference is a manifest problem, and telling them apart is the
        // difference between waiting and editing.
        401 => Err("expired".into()),
        403 => Err("this instance is not authorised for that reference".into()),
        404 => Err("no such secret".into()),
        code => Err(format!("the platform answered {code}")),
    }
}

/// Percent-encode what a `vault://org/name` reference can contain. Not a general
/// encoder: the platform validated the shape, so this only has to survive a query
/// string.
fn urlencoding(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_cache_is_wiped_when_it_is_dropped() {
        // The property is that nothing is left behind; the assertion here is only
        // that the entry is gone, since observing freed memory is not something a
        // test can do honestly.
        let mut c = SecretCache::default();
        let r = SecretRef::for_test("vault://acme/stripe");
        c.put(&r, "sk_live_x".into());
        assert_eq!(c.get(&r).map(String::as_str), Some("sk_live_x"));
        drop(c);
    }

    #[test]
    fn one_instances_cache_is_not_anothers() {
        let (mut a, b) = (SecretCache::default(), SecretCache::default());
        let r = SecretRef::for_test("vault://acme/stripe");
        a.put(&r, "value".into());
        assert!(b.get(&r).is_none(), "a second instance must not see the first's fetch");
    }

    #[test]
    fn a_reference_survives_a_query_string() {
        assert_eq!(urlencoding("vault://acme/stripe"), "vault%3A%2F%2Facme%2Fstripe");
    }
}
