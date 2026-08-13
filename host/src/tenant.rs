//! Who a running instance is, and what it is allowed to touch.
//!
//! One `comp-host` process holds every tenant on a node, so the types in this file
//! are the tenant boundary. There is no OS process between two tenants here — only
//! wasmtime's sandbox and the discipline below. That makes this the one module to
//! be paranoid in.
//!
//! The rule everything obeys:
//!
//! > A name is a real boundary iff **(1)** it is chosen by host-side state the
//! > guest cannot write, and **(2)** the guest has no second path into the
//! > namespace.
//!
//! wasmCloud failed both, which is what ADR-0012 measured: `store::open(name)` let
//! the *guest* pick the bucket, and the bus let any component reach any bucket.
//! Operationally that reduces to one sentence, and it is the thing to grep for
//! before adding any capability here:
//!
//! > **No capability impl may use a guest-supplied string as a namespace
//! > selector.** A guest string may only be a lookup key into a host-side
//! > allow-list.
//!
//! `BucketId` makes that a compile-time property rather than a habit: its field is
//! private to this module, so `kv.rs` cannot be handed anything a guest said.

use std::collections::{BTreeMap, BTreeSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ---- identity --------------------------------------------------------------

/// The real name of a store, as chosen by the host.
///
/// The inner field is deliberately private. Nothing outside this module can build
/// one, so a `BucketId` reaching a backend is proof that the host named it — which
/// is clause (1), enforced by the compiler instead of by review.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct BucketId(String);

/// A secret reference, as the PLATFORM wrote it — `vault://<org>/<name>`.
///
/// Private field for the same reason as `BucketId`: nothing outside this module can
/// build one, so a `SecretRef` reaching the fetch path is proof it came from a
/// manifest the platform validated. A guest string can never become one.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct SecretRef(String);

impl SecretRef {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[cfg(test)]
    pub fn for_test(r: &str) -> Self {
        SecretRef(r.to_string())
    }
}

impl BucketId {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Backend tests need to name a store without going through a `Scope`.
    /// `cfg(test)` on purpose: the production build has no way to make one of
    /// these except from a scope, which is the property being defended.
    #[cfg(test)]
    pub fn for_test(name: &str) -> Self {
        BucketId(name.to_string())
    }
}

impl std::fmt::Display for BucketId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// `<tenant>/<app>/<component>` — how an instance is addressed everywhere: the
/// instance table, the link tables, and the lattice subject.
pub type InstanceId = String;

pub fn instance_id(tenant: &str, app: &str, component: &str) -> InstanceId {
    format!("{tenant}/{app}/{component}")
}

// ---- naming (salvaged from the renderer) -----------------------------------

pub fn dns_label(s: &str) -> String {
    let out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    out.trim_matches('-').to_string()
}

/// One application's identity across the fleet. Derived from tenant + app, never
/// supplied — a tenant able to set this would be a tenant able to name someone
/// else's storage.
/// One application's identity across the fleet. Derived from tenant + app, never
/// supplied — a tenant able to set this would be a tenant able to name someone
/// else's storage.
///
/// The 53-character cap is a real constraint, and plain truncation was a silent
/// isolation break. Environments nest (`shop-env-a-env-b`, ADR-0078), so names
/// grow by six characters per level, and past the cap two SIBLINGS differing only
/// in their last segment truncate to the same string — one store, two
/// environments, each reading the other's writes. With single-character names it
/// happens at depth seven, and nothing anywhere would have said so.
///
/// So an over-long name keeps a readable prefix and earns a suffix derived from
/// the WHOLE name. Names that fit are untouched, which keeps the common case
/// legible and every case distinct.
pub fn env_for(tenant: &str, app: &str) -> String {
    const CAP: usize = 53;
    /// 8 hex characters plus the separator.
    const SUFFIX: usize = 9;

    let full = format!("app-{}-{}", dns_label(tenant), dns_label(app));
    if full.len() <= CAP {
        return full.trim_matches('-').to_string();
    }
    let mut h = Sha256::new();
    h.update(full.as_bytes());
    let digest = h.finalize();
    let tag: String = digest.iter().take(4).map(|b| format!("{b:02x}")).collect();
    let head: String = full.chars().take(CAP - SUFFIX).collect();
    format!("{}-{tag}", head.trim_end_matches('-'))
}

// ---- egress ----------------------------------------------------------------


/// What one app may dial.
///
/// Two independent checks, on purpose. The allow-list is on **names**, because
/// that is what an operator can reason about. The deny-list is on **resolved
/// addresses**, because a name check alone is satisfied by pointing an
/// allow-listed name at the metadata endpoint.
#[derive(Debug, Clone, Default)]
pub struct EgressPolicy {
    allowed: BTreeSet<String>,
    unrestricted: bool,
    /// Dev escape hatch. Off means a tenant cannot reach any private network,
    /// which on a Tailscale node includes every other node in the lattice.
    allow_private: bool,
    /// Sockets this node knows are dangerous regardless of range — its own
    /// listener and the NATS it is joined to, which may be public.
    ///
    /// A SOCKET, not an address: the danger is the port this host serves on, and
    /// denying the whole IP would also deny every unrelated service that happens
    /// to share it. On a lattice node that distinction is invisible, because the
    /// IP is private and denied by range anyway — it only shows up under
    /// `--allow-private-egress`, where it made a database on loopback
    /// unreachable while claiming to protect the listener.
    denied: BTreeSet<SocketAddr>,
}

impl EgressPolicy {
    pub fn new(allow: &[String], allow_private: bool, denied: &[SocketAddr]) -> Self {
        Self {
            unrestricted: allow.iter().any(|a| a.trim() == "*"),
            allowed: expand_egress(allow).into_iter().collect(),
            allow_private,
            denied: denied.iter().copied().collect(),
        }
    }

    /// Clause (2) for the network: nothing is reachable unless it was named.
    /// Empty means deny-all, which is the correct reading of "no egress declared".
    pub fn permits_authority(&self, authority: &str) -> bool {
        if self.unrestricted {
            return true;
        }
        let a = authority.trim().to_ascii_lowercase();
        // Both forms are checked because both are emitted — see `expand_egress`.
        self.allowed.contains(&a)
            || a.rsplit_once(':')
                .map(|(host, port)| {
                    port.chars().all(|c| c.is_ascii_digit()) && self.allowed.contains(host)
                })
                .unwrap_or(false)
    }

    /// Addresses no tenant may reach, whatever the allow-list says.
    ///
    /// Deliberately not overridable per app: the allow-list is the tenant-facing
    /// knob, and this is the backstop under it. An operator who genuinely needs an
    /// internal target puts a reverse proxy on an allow-listed public name, or runs
    /// the host with `--allow-private-egress` and accepts what that means.
    pub fn permits_addr(&self, addr: SocketAddr) -> bool {
        if self.denied.contains(&addr) {
            return false;
        }
        if self.allow_private {
            return true;
        }
        !is_private(addr.ip())
    }
}

/// Ranges that are a lateral move rather than an outbound request.
fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()      // 169.254/16 — cloud metadata lives here
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_unspecified()
                // Tailscale's CGNAT range: reaching it is reaching the lattice.
                || (o[0] == 100 && (64..=127).contains(&o[1]))
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            v6.is_loopback()
                || v6.is_multicast()
                || v6.is_unspecified()
                || (seg[0] & 0xffc0) == 0xfe80 // link-local
                || (seg[0] & 0xfe00) == 0xfc00 // unique-local
                // v4-mapped, so ::ffff:169.254.169.254 is not a way around the above.
                || v6.to_ipv4_mapped().map(|m| is_private(IpAddr::V4(m))).unwrap_or(false)
        }
    }
}

/// Expand an egress allow-list into the forms an authority can actually arrive in.
///
/// Salvaged verbatim from the renderer, including its reasons:
///
/// * A scheme-qualified entry is passed through untouched. Splitting it on `:` to
///   find a port would turn `https://api.example.com` into the host `https` —
///   silently allow-listing the wrong thing, since `https` is itself a legal host.
/// * A bare authority is emitted both bare and port-qualified, because egress is
///   fail-closed and a missing form is a connection refused at runtime rather than
///   an error at deploy.
fn expand_egress(egress: &[String]) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for e in egress {
        let e = e.trim().to_ascii_lowercase();
        if e.is_empty() || e == "*" {
            continue;
        }
        if e.contains("://") {
            out.insert(e);
            continue;
        }
        match e.rsplit_once(':') {
            // A trailing `:digits` is a port; keep both forms.
            Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
                out.insert(host.to_string());
                out.insert(e.clone());
            }
            // Otherwise a bare authority (or something with a colon that is not a
            // port, which we do not try to be clever about).
            _ => {
                out.insert(format!("{e}:80"));
                out.insert(format!("{e}:443"));
                out.insert(e);
            }
        }
    }
    out.into_iter().collect()
}

// ---- the scope -------------------------------------------------------------

/// Everything one instance is allowed to see, built once from a start command and
/// immutable thereafter.
///
/// Never from guest input. Never from process environment — in a shared process
/// `std::env` is a cross-tenant read by construction, which is why the old
/// `build_config()` is gone rather than scoped.
#[derive(Debug, Clone)]
pub struct Scope {
    pub tenant: String,
    pub app: String,
    pub component: String,
    pub digest: String,
    /// Guest-visible store name -> the real store. THE ADR-0012 fix: a guest string
    /// is a key into this map and can never be a bucket name.
    buckets: BTreeMap<String, BucketId>,
    pub cfg: BTreeMap<String, String>,
    /// Guest-visible key -> the reference the platform granted. The guest asks for
    /// `"stripe"`; it cannot ask for `vault://globex/stripe`, because it cannot
    /// name one at all (ADR-0051).
    secrets: BTreeMap<String, SecretRef>,
    /// Authorises fetching exactly the references above, for a bounded time. A
    /// capability, not a secret: it is worth what this manifest was worth, which is
    /// why it can live in the ledger on disk (ADR-0022).
    pub fetch_token: String,
    pub egress: EgressPolicy,
    /// `import iface -> instance id`. An import with no entry and no host impl
    /// means the instance refuses to start, so omission fails closed.
    pub links: BTreeMap<String, InstanceId>,
    pub mem_cap: usize,
    pub slice_ms: u64,
}

impl Scope {
    pub fn id(&self) -> InstanceId {
        instance_id(&self.tenant, &self.app, &self.component)
    }

    /// The whole of `store::open`'s policy.
    ///
    /// `None` is a refusal, not a fallback. A fallback — "unknown name, use the
    /// default" — would re-introduce exactly what ADR-0012 measured, because a
    /// guest naming a neighbour's bucket would get *a* bucket rather than an error.
    pub fn bucket(&self, guest_name: &str) -> Option<&BucketId> {
        self.buckets.get(guest_name)
    }

    #[cfg(test)]
    pub fn bucket_names(&self) -> Vec<&str> {
        self.buckets.keys().map(|s| s.as_str()).collect()
    }

    /// The whole of `reader::get`'s policy, and the twin of `bucket` above.
    ///
    /// `None` is "you were not granted that key", which the WIT models as `none`
    /// rather than an error — an optional secret being absent is a normal way to
    /// run. What it is NOT is a lookup a guest can widen: the key indexes host-side
    /// state, so a guest naming another tenant's reference gets nothing, because it
    /// cannot name a reference at all.
    pub fn secret(&self, guest_key: &str) -> Option<&SecretRef> {
        self.secrets.get(guest_key)
    }

    /// Every reference this instance was granted, for the start-time existence
    /// check. Refs only — the values are not fetched here and may never be.
    pub fn secret_refs(&self) -> impl Iterator<Item = (&str, &SecretRef)> {
        self.secrets.iter().map(|(k, r)| (k.as_str(), r))
    }
}

/// A start command, as the reconciler emits it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StartCommand {
    pub tenant: String,
    pub app: String,
    pub component: String,
    pub digest: String,
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    #[serde(default)]
    pub links: BTreeMap<String, String>,
    /// `key -> vault://<org>/<name>`, validated by the platform at save (ADR-0050).
    #[serde(default)]
    pub secrets: BTreeMap<String, String>,
    /// Authorises fetching exactly those references, and nothing else.
    #[serde(default)]
    pub fetch_token: String,
    #[serde(default)]
    pub host_needs: Vec<String>,
    /// Stamped by the platform, never authored by a tenant (ADR-0008).
    #[serde(default)]
    pub egress: Vec<String>,
    /// The Host header this instance answers to, if it is the one serving HTTP.
    #[serde(default)]
    pub ingress_host: Option<String>,
}

fn one() -> u32 {
    1
}

/// Host-wide defaults for the knobs a start command does not carry.
// ponytail: fleet-wide limits; per-app limits when one tenant genuinely needs a
// bigger ceiling than its neighbours.
#[derive(Debug, Clone)]
pub struct Limits {
    pub mem_cap: usize,
    pub slice_ms: u64,
    pub allow_private_egress: bool,
    pub denied_addrs: Vec<SocketAddr>,
}

impl StartCommand {
    pub fn into_scope(self, limits: &Limits) -> Scope {
        // The seeded name is `default` because that is what every component in the
        // catalog hardcodes. Seeding it means the ADR-0012 fix needs zero catalog
        // changes — which is precisely what killed the fix proposed there.
        let real = BucketId(format!("b-{}", env_for(&self.tenant, &self.app)));
        let buckets = BTreeMap::from([("default".to_string(), real)]);

        Scope {
            buckets,
            // Wrapped here and nowhere else: this is the only place a string from a
            // start command becomes a `SecretRef`, which is what makes the newtype
            // a boundary rather than a label.
            secrets: self.secrets.into_iter().map(|(k, r)| (k, SecretRef(r))).collect(),
            fetch_token: self.fetch_token,
            egress: EgressPolicy::new(
                &self.egress,
                limits.allow_private_egress,
                &limits.denied_addrs,
            ),
            cfg: self.config,
            links: self.links,
            mem_cap: limits.mem_cap,
            slice_ms: limits.slice_ms,
            tenant: self.tenant,
            app: self.app,
            component: self.component,
            digest: self.digest,
        }
    }
}

pub type SharedScope = Arc<Scope>;

#[cfg(test)]
mod tests {
    use super::*;

    fn scope(tenant: &str, app: &str) -> Scope {
        StartCommand {
            secrets: Default::default(),
            fetch_token: String::new(),
            tenant: tenant.into(),
            app: app.into(),
            component: "api".into(),
            digest: "sha256:a".into(),
            count: 1,
            config: BTreeMap::new(),
            links: BTreeMap::new(),
            host_needs: vec![],
            egress: vec![],
            ingress_host: None,
        }
        .into_scope(&limits())
    }

    fn limits() -> Limits {
        Limits {
            mem_cap: 64 << 20,
            slice_ms: 50,
            allow_private_egress: false,
            denied_addrs: vec![],
        }
    }

    /// ADR-0012, as a test. Two tenants, the same guest code, the same hardcoded
    /// bucket name — and no path from one to the other.
    #[test]
    fn two_tenants_opening_the_same_name_get_different_stores() {
        let alice = scope("alice", "shop");
        let eve = scope("eve", "shop");
        let a = alice.bucket("default").expect("alice's default");
        let e = eve.bucket("default").expect("eve's default");
        assert_ne!(a, e, "the same guest string must not name the same store");
        assert_eq!(a.as_str(), "b-app-alice-shop");
        assert_eq!(e.as_str(), "b-app-eve-shop");
    }

    /// The exact call that leaked. A guest that names anything else gets nothing —
    /// not a default, not an empty bucket. A fallback here would rebuild the bug.
    #[test]
    fn opening_an_unlisted_name_is_a_refusal_not_a_fallback() {
        let alice = scope("alice", "shop");
        for hostile in [
            "b-app-eve-shop",  // eve's real store, named directly
            "eve",
            "app-eve-shop",
            "",
            "DEFAULT",         // case must not be a way in
            "default ",
            "../default",
        ] {
            assert!(
                alice.bucket(hostile).is_none(),
                "opening {hostile:?} must refuse, not resolve"
            );
        }
        // ...and the one legitimate name still works.
        assert!(alice.bucket("default").is_some());
    }

    #[test]
    fn a_scope_grants_exactly_one_store() {
        // If this ever grows, every new entry is a new thing a guest can name.
        assert_eq!(scope("alice", "shop").bucket_names(), vec!["default"]);
    }

    #[test]
    fn tenant_names_that_collide_after_sanitising_still_separate() {
        // `dns_label` maps punctuation to '-', so two different tenant strings can
        // sanitise to the same label. Worth knowing about explicitly rather than
        // discovering as a leak.
        let a = scope("a.b", "shop");
        let b = scope("a_b", "shop");
        assert_eq!(a.bucket("default"), b.bucket("default"),
            "sanitising collides — tenant ids must be validated as DNS labels upstream");
    }

    #[test]
    fn egress_is_deny_all_when_nothing_is_declared() {
        let p = EgressPolicy::new(&[], false, &[]);
        for target in ["example.com", "example.com:443", "127.0.0.1:4222"] {
            assert!(!p.permits_authority(target), "{target} must be refused");
        }
    }

    #[test]
    fn egress_accepts_both_the_bare_and_port_qualified_forms() {
        // The lesson from jobs.yaml: the same host arrives written both ways, and
        // egress is fail-closed, so a missing form is a mystery at runtime.
        let p = EgressPolicy::new(&["api.stripe.com".into()], false, &[]);
        assert!(p.permits_authority("api.stripe.com"));
        assert!(p.permits_authority("api.stripe.com:443"));
        assert!(p.permits_authority("api.stripe.com:80"));
        assert!(p.permits_authority("API.STRIPE.COM:443"), "authority casing is not significant");
        assert!(!p.permits_authority("evil.com"));
        assert!(!p.permits_authority("notapi.stripe.com"));
        // A suffix must not be a way in.
        assert!(!p.permits_authority("api.stripe.com.evil.com"));
    }

    #[test]
    fn a_port_qualified_entry_still_allows_the_bare_host() {
        let p = EgressPolicy::new(&["registry.internal:5000".into()], false, &[]);
        assert!(p.permits_authority("registry.internal:5000"));
        assert!(p.permits_authority("registry.internal"));
    }

    #[test]
    fn a_scheme_qualified_entry_is_not_split_on_its_colon() {
        // Splitting this on ':' would allow-list the host `https`, which is legal
        // and would be a very quiet hole.
        let p = EgressPolicy::new(&["https://api.example.com".into()], false, &[]);
        assert!(!p.permits_authority("https"));
        assert!(p.permits_authority("https://api.example.com"));
    }

    #[test]
    fn star_is_the_documented_opt_out() {
        let p = EgressPolicy::new(&["*".into()], false, &[]);
        assert!(p.permits_authority("anything.at.all:9999"));
        // ...but it does not unlock the address deny-list. Unrestricted names are
        // still not unrestricted networks.
        assert!(!p.permits_addr("169.254.169.254:80".parse().unwrap()));
    }

    /// The lateral-movement list. Every one of these is reachable from a node and
    /// none of them is an outbound request.
    #[test]
    fn the_address_deny_list_covers_every_way_off_the_box() {
        let p = EgressPolicy::new(&["*".into()], false, &[]);
        for bad in [
            "127.0.0.1",        // the node's own listener, the NATS bus
            "0.0.0.0",
            "169.254.169.254",  // cloud metadata — credentials
            "10.0.0.5",
            "172.16.0.1",
            "192.168.1.1",
            "100.64.0.1",       // Tailscale CGNAT: the rest of the lattice
            "100.127.255.255",
            "::1",
            "fe80::1",
            "fc00::1",
            "::ffff:169.254.169.254", // v4-mapped, the obvious way around the above
        ] {
            assert!(!p.permits_addr(sock(bad)), "{bad} must be prohibited");
        }
        // Ordinary public addresses are fine, including ones adjacent to the ranges.
        for ok in ["93.184.216.34", "100.63.255.255", "100.128.0.1", "2606:2800:220:1::"] {
            assert!(p.permits_addr(sock(ok)), "{ok} must be allowed");
        }
    }

    /// A bare address, at the port a plain HTTP dial would use.
    fn sock(ip: &str) -> SocketAddr {
        SocketAddr::new(ip.parse().unwrap(), 80)
    }

    #[test]
    fn an_explicitly_denied_socket_survives_allow_private() {
        // The dev escape hatch must not re-open the bus this host is joined to.
        let nats: SocketAddr = "10.1.2.3:4222".parse().unwrap();
        let p = EgressPolicy::new(&["*".into()], true, &[nats]);
        assert!(!p.permits_addr(nats));
        assert!(p.permits_addr("10.1.2.4:4222".parse().unwrap()), "allow_private otherwise applies");
        // The DENY is the socket, not the machine. Under `--allow-private-egress`
        // a database sharing an address with the bus stays reachable — denying the
        // whole IP was what made a loopback SurrealDB undialable while the thing
        // actually being protected was one port.
        assert!(p.permits_addr("10.1.2.3:8000".parse().unwrap()), "another port on the same host");
    }


    /// The bug this function was written with, and the reason it now hashes.
    ///
    /// Environments nest, names grow six characters a level, and plain truncation
    /// made two siblings share a bucket — silently, with each reading the other's
    /// writes. This walks the nesting down and asserts that never happens.
    #[test]
    fn nested_environments_never_share_a_bucket() {
        let mut chain = "graph".to_string();
        let mut seen = std::collections::BTreeSet::new();
        for depth in 0..12 {
            let a = env_for("ada", &format!("{chain}-env-a"));
            let b = env_for("ada", &format!("{chain}-env-b"));
            assert_ne!(
                a, b,
                "at depth {depth} two SIBLING environments share one bucket — each would \
                 read the other's writes, and nothing would say so"
            );
            assert!(a.len() <= 53, "at depth {depth} the name is {} chars: {a}", a.len());
            assert!(seen.insert(a.clone()), "depth {depth} collided with an ancestor: {a}");
            assert!(seen.insert(b), "depth {depth} collided with an ancestor");
            chain = format!("{chain}-env-x");
        }
    }

    /// A name that fits is left exactly as it was — the hash is a fallback, not a
    /// rename, and existing stores must keep their names.
    #[test]
    fn a_name_that_fits_is_not_rewritten() {
        assert_eq!(env_for("ada", "graph"), "app-ada-graph");
        assert_eq!(env_for("ada", "graph-env-node-7"), "app-ada-graph-env-node-7");
    }

    #[test]
    fn an_app_name_is_derived_and_length_capped() {
        assert_eq!(env_for("alice", "shop"), "app-alice-shop");
        // Each half is trimmed before joining, so trailing punctuation does not
        // leave a doubled separator.
        assert_eq!(env_for("Alice Co.", "My Shop!"), "app-alice-co-my-shop");
        let long = env_for(&"t".repeat(80), &"a".repeat(80));
        assert!(long.len() <= 53 && !long.ends_with('-'), "{long}");
    }
}
