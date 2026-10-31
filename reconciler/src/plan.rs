//! Pure diff of desired vs observed. Assumes `desired` is complete: a partial list stops apps.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Strategy {
    /// One artifact, `wac plug`-composed at save. Only the root exists at runtime.
    #[default]
    Fused,
    /// N components, runtime-linked by the host from a link table.
    #[serde(alias = "split")]
    Linked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
    /// `replicas` distributed over the eligible nodes.
    #[default]
    Spread,
    /// One per eligible node; `replicas` is ignored.
    Daemon,
    Pinned,
}

/// `target` is concurrent requests per replica, not a rate: a rate over a wrong window oscillates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scale {
    /// `0` parks the app; a request brings it back (ADR-0042).
    pub min: u32,
    pub max: u32,
    /// Concurrent requests one replica should carry before another is added.
    #[serde(default = "one")]
    pub target: u32,
}

pub type Load = BTreeMap<String, u32>;

/// Reported because autoscaling cannot fix it; a fleet pinned at `max` would look correctly sized.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AtCeiling {
    pub tenant: String,
    pub app: String,
    pub component: String,
    pub max: u32,
    /// Before clamping.
    pub wanted: u32,
}

/// Separate from placement so "how many" can be tested without a fleet.
pub fn desired_replicas(c: &Component, m: &Manifest, load: Option<&Load>, running: u32) -> u32 {
    desired_and_wanted(c, m, load, running).0
}

/// The clamped count and what demand actually asked for ("4 is right" vs "4 is all you allowed").
fn desired_and_wanted(
    c: &Component,
    m: &Manifest,
    load: Option<&Load>,
    running: u32,
) -> (u32, u32) {
    let Some(scale) = &c.scale else {
        return (c.replicas, c.replicas);
    };
    let hold = |n: u32| {
        let lo = scale.min;
        let hi = scale.max.max(scale.min);
        (n.clamp(lo, hi), n)
    };
    // No signal is not no traffic: hold what runs (ADR-0022: a failed poll changes nothing).
    let Some(load) = load else {
        return hold(running.max(scale.min));
    };
    // Not published yet: falling to `min` would scale a busy app to zero on every ingress restart.
    let Some(host) = m.ingress.as_ref().map(|i| i.host.as_str()) else {
        return hold(running.max(scale.min));
    };
    // No sample yet: hold, do not collapse.
    let Some(&demand) = load.get(host) else {
        return hold(running.max(scale.min));
    };
    let target = scale.target.max(1);
    // Round up: at target 10, eleven concurrent requests need two replicas, not one.
    let wanted = demand.div_ceil(target);
    (wanted.clamp(scale.min, scale.max.max(scale.min)), wanted)
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    #[serde(default)]
    pub cpus: usize,
}

impl Capacity {
    /// Never zero: a node advertising nothing must stay placeable and not divide by zero.
    pub fn weight(&self) -> usize {
        self.cpus.max(1)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    #[serde(default)]
    pub mode: Mode,
    #[serde(default)]
    pub nodes: Vec<String>,
    /// Equality only.
    // ponytail: equality only; add operators when a real multiregion query needs them.
    #[serde(default)]
    pub constraints: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Component {
    pub id: String,
    /// Content address of the artifact. Identity, not a tag (ADR-0006).
    pub digest: String,
    #[serde(default = "one")]
    pub replicas: u32,
    /// `None` means fixed `replicas`; a defaulted `min: 1, max: 1` reads as pinned autoscaling.
    #[serde(default)]
    pub scale: Option<Scale>,
    #[serde(default)]
    pub placement: Placement,
    /// Stamped at save, never authored, so placement is a set comparison.
    #[serde(default)]
    pub host_needs: Vec<String>,
    #[serde(default)]
    pub config: BTreeMap<String, String>,
    /// By reference only. A value must never reach a manifest (ADR-0010).
    #[serde(default)]
    pub secrets: Vec<SecretRef>,
    /// Empty is deny-all. Stamped by the platform, never authored by a tenant (ADR-0008).
    #[serde(default)]
    pub egress: Vec<String>,
}

fn one() -> u32 {
    1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretRef {
    pub key: String,
    #[serde(rename = "ref")]
    pub reference: String,
}

/// `composer.edge` verbatim, so the subtype checker validates it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Link {
    pub plug: String,
    pub socket: String,
    pub iface: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ingress {
    pub host: String,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub app: String,
    pub tenant: String,
    pub strategy: Strategy,
    pub components: Vec<Component>,
    #[serde(default)]
    pub links: Vec<Link>,
    #[serde(default)]
    pub ingress: Option<Ingress>,
}

impl Manifest {
    /// The rest of the app follows this component (see `place`).
    fn root(&self) -> Option<&Component> {
        match &self.ingress {
            Some(i) => self.components.iter().find(|c| c.id == i.component),
            None => self.components.first(),
        }
    }

    /// All local, as the graph is co-located: the host binds direct in-process calls.
    fn link_table(&self, socket: &str) -> BTreeMap<String, String> {
        self.links
            .iter()
            .filter(|l| l.socket == socket)
            .map(|l| (l.iface.clone(), format!("{}/{}/{}", self.tenant, self.app, l.plug)))
            .collect()
    }

    /// Refused, not resolved: imports are keyed by interface, so the last link would silently win.
    /// ponytail: revisit if named-import linking becomes available (a name per link).
    fn conflicting_links(&self) -> Vec<String> {
        let mut seen: BTreeMap<(&str, &str), Vec<&str>> = BTreeMap::new();
        for l in &self.links {
            seen.entry((l.socket.as_str(), l.iface.as_str())).or_default().push(l.plug.as_str());
        }
        seen.into_iter()
            .filter(|(_, plugs)| plugs.len() > 1)
            .map(|((socket, iface), plugs)| {
                format!(
                    "`{socket}` is given `{iface}` by {} — an import is keyed by \
                     interface, so these cannot be told apart; keep one",
                    plugs.iter().map(|p| format!("`{p}`")).collect::<Vec<_>>().join(" and ")
                )
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningInstance {
    pub tenant: String,
    pub app: String,
    pub component: String,
    pub digest: String,
    #[serde(default = "one")]
    pub count: u32,
    /// Lets an ingress route from inventory alone while the control plane is down.
    #[serde(default)]
    pub ingress_host: Option<String>,
}

/// A full snapshot, not a delta: a few KB, idempotent, and a new reconciler learns all in one beat.
// ponytail: full snapshot; deltas + periodic full sync once one exceeds NATS max payload.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct NodeInventory {
    pub node: String,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Host capabilities this node can grant, e.g. `wasi:keyvalue/store`.
    #[serde(default)]
    pub host_ifaces: Vec<String>,
    /// False by default: an unknown node must read as node-local, or stateful replicas diverge.
    #[serde(default)]
    pub kv_shared: bool,
    /// A node bound to `0.0.0.0` knows its port, not its address.
    #[serde(default)]
    pub address: String,
    #[serde(default)]
    pub capacity: Capacity,
    #[serde(default)]
    pub instances: Vec<RunningInstance>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verb", rename_all = "lowercase")]
pub enum Command {
    Start {
        node: String,
        tenant: String,
        app: String,
        component: String,
        digest: String,
        count: u32,
        config: BTreeMap<String, String>,
        secrets: Vec<SecretRef>,
        links: BTreeMap<String, String>,
        host_needs: Vec<String>,
        egress: Vec<String>,
        /// `None` for a plug: it is reachable through links.
        ingress_host: Option<String>,
    },
    Stop {
        node: String,
        tenant: String,
        app: String,
        component: String,
        digest: String,
        count: u32,
    },
}

impl Command {
    pub fn node(&self) -> &str {
        match self {
            Command::Start { node, .. } | Command::Stop { node, .. } => node,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unschedulable {
    pub tenant: String,
    pub app: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub commands: Vec<Command>,
    pub unschedulable: Vec<Unschedulable>,
    /// Components that wanted more replicas than `max` allows.
    #[serde(default)]
    pub at_ceiling: Vec<AtCeiling>,
    /// Never silently truncated: a dropped command would read as "converged".
    pub deferred: usize,
    /// Replicas this plan means to run, every component counted as placed: what the fleet's lag
    /// is measured against. Not `replicas` summed, which an autoscaled or scaled-to-zero app ignores.
    #[serde(default)]
    pub wanted: u64,
}

#[derive(Debug, Clone)]
pub struct Cfg {
    /// Consecutive passes a surplus must persist before anything is stopped.
    pub settle_passes: u32,
    /// Commands emitted per pass, so a mass event drains instead of stampeding.
    pub max_commands: usize,
    /// A switch so the differential test can prove both paths agree, and to rule it out live.
    pub fast_path: bool,
}

impl Default for Cfg {
    fn default() -> Self {
        // Guesses until there is real churn to calibrate against; the binary exposes them as flags.
        Self { settle_passes: 2, max_commands: 20, fast_path: true }
    }
}

/// Only removal waits: under-replicated fires on the first pass.
#[derive(Debug, Clone, Default)]
pub struct Hysteresis {
    seen: BTreeMap<Key, u32>,
    /// Hashed fleet shape: when it changes, every app takes the full ranking for one pass.
    fleet: Option<u64>,
}

type Key = (String, String, String, String, String); // tenant, app, component, digest, node
/// Borrowed: allocating here once per app per pass was 300 ms vs 46 ms.
type Owner<'a> = (&'a str, &'a str, &'a str, &'a str);

type EligibleFor<'a> =
    BTreeMap<(&'a BTreeMap<String, String>, &'a Vec<String>), Vec<&'a NodeInventory>>;

pub fn plan(
    desired: &[Manifest],
    observed: &[NodeInventory],
    load: Option<&Load>,
    hyst: &mut Hysteresis,
    cfg: &Cfg,
) -> Outcome {
    let mut out = Outcome::default();
    let mut want: BTreeMap<Key, (u32, &Manifest, &Component)> = BTreeMap::new();

    // Indexed once: a scan per manifest cost 1.76 s at 10 000 apps (ADR-0055). Keyed by tenant and
    // app too, or two tenants deploying one component count each other's replicas.
    let mut have: BTreeMap<Key, u32> = BTreeMap::new();
    let mut node_load: BTreeMap<&str, usize> = BTreeMap::new();
    for inv in observed {
        node_load.insert(&inv.node, 0);
        for i in &inv.instances {
            *have
                .entry((
                    i.tenant.clone(),
                    i.app.clone(),
                    i.component.clone(),
                    i.digest.clone(),
                    inv.node.clone(),
                ))
                .or_default() += i.count;
            *node_load.get_mut(inv.node.as_str()).unwrap() += i.count as usize;
        }
    }
    // Borrowed keys: ranking a node is a `&str` lookup, not five allocations.
    let mut running_by_node: BTreeMap<Owner<'_>, BTreeMap<&str, u32>> = BTreeMap::new();
    let mut running_total: BTreeMap<Owner<'_>, u32> = BTreeMap::new();
    for ((t, a, c, d, node), n) in &have {
        let owner = (t.as_str(), a.as_str(), c.as_str(), d.as_str());
        running_by_node.entry(owner).or_default().insert(node.as_str(), *n);
        *running_total.entry(owner).or_default() += n;
    }
    let by_node: BTreeMap<&str, &NodeInventory> =
        observed.iter().map(|n| (n.node.as_str(), n)).collect();

    // Memoised: almost every app shares an eligibility signature.
    let mut eligible_for: EligibleFor<'_> = BTreeMap::new();
    let fitting = |c: &Component, node: &str| by_node.get(node).is_some_and(|n| fits(c, n));

    // Composition, not load: a node joining or relabelling disables the fast path; traffic not.
    let fleet = {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for n in observed {
            n.node.hash(&mut h);
            n.labels.hash(&mut h);
            n.host_ifaces.hash(&mut h);
            n.capacity.cpus.hash(&mut h);
            n.kv_shared.hash(&mut h);
        }
        h.finish()
    };
    let fleet_stable = hyst.fleet == Some(fleet);
    hyst.fleet = Some(fleet);

    // Load already committed this pass, so later apps see earlier ones instead of racing to one node.
    let mut pending: BTreeMap<String, usize> = BTreeMap::new();

    for m in desired {
        let Some(root) = m.root() else {
            out.unschedulable.push(Unschedulable {
                tenant: m.tenant.clone(),
                app: m.app.clone(),
                reason: "manifest has no components".into(),
            });
            continue;
        };
        // Ambiguous wiring is refused before anything is placed: half a link table looks like it worked.
        let conflicts = m.conflicting_links();
        if !conflicts.is_empty() {
            out.unschedulable.push(Unschedulable {
                tenant: m.tenant.clone(),
                app: m.app.clone(),
                reason: conflicts.join("; "),
            });
            continue;
        }

        let running: u32 = running_total
            .get(&(m.tenant.as_str(), m.app.as_str(), root.id.as_str(), root.digest.as_str()))
            .copied()
            .unwrap_or(0);
        let (want_replicas, wanted) = desired_and_wanted(root, m, load, running);
        if let Some(scale) = &root.scale {
            if wanted > scale.max.max(scale.min) {
                out.at_ceiling.push(AtCeiling {
                    tenant: m.tenant.clone(),
                    app: m.app.clone(),
                    component: root.id.clone(),
                    max: scale.max.max(scale.min),
                    wanted,
                });
            }
        }
        let owner = (m.tenant.as_str(), m.app.as_str(), root.id.as_str(), root.digest.as_str());
        let nodes = match settled(
            root,
            want_replicas,
            running,
            running_by_node.get(&owner),
            &fitting,
            fleet_stable,
        ) {
            Some(existing) => existing,
            None => match place(
                root,
                (&m.tenant, &m.app),
                want_replicas,
                observed,
                &running_by_node,
                &node_load,
                &mut eligible_for,
                &pending,
            ) {
                Ok(n) => n,
                Err(reason) => {
                    out.unschedulable.push(Unschedulable {
                        tenant: m.tenant.clone(),
                        app: m.app.clone(),
                        reason,
                    });
                    continue;
                }
            },
        };

        // Node-local stores give each replica its own store under one name; refused (ADR-0013).
        if nodes.len() > 1 && holds_state(m) {
            let local: Vec<&str> = nodes
                .iter()
                .filter(|(n, _)| !observed.iter().any(|o| o.node == *n && o.kv_shared))
                .map(|(n, _)| n.as_str())
                .collect();
            if !local.is_empty() {
                out.unschedulable.push(Unschedulable {
                    tenant: m.tenant.clone(),
                    app: m.app.clone(),
                    reason: format!(
                        "spread across {} nodes but {local:?} have node-local stores — every \
                         replica would get its own store under the same bucket name and \
                         diverge in silence. Use replicas: 1, or run those nodes with a \
                         shared backend (--kv nats).",
                        nodes.len()
                    ),
                });
                continue;
            }
        }
        for (node, count) in &nodes {
            want.insert(key(m, &root.id, &root.digest, node), (*count, m, root));
            *pending.entry(node.clone()).or_default() += *count as usize;
        }
        // The graph follows the root onto the same nodes, keeping every link local.
        // ponytail: whole-graph co-location; slice two honours per-component placement.
        if m.strategy == Strategy::Linked {
            for c in m.components.iter().filter(|c| c.id != root.id) {
                // A plug with its own placement goes there (a GPU box); its link becomes wRPC.
                let spans = !c.placement.constraints.is_empty()
                    || c.placement.mode == Mode::Pinned
                    || !c.placement.nodes.is_empty();
                let targets = if spans {
                    match place(
                        c,
                        (&m.tenant, &m.app),
                        c.replicas,
                        observed,
                        &running_by_node,
                        &node_load,
                        &mut eligible_for,
                        &pending,
                    ) {
                        Ok(n) => n,
                        Err(reason) => {
                            out.unschedulable.push(Unschedulable {
                                tenant: m.tenant.clone(),
                                app: m.app.clone(),
                                reason: format!("`{}`: {reason}", c.id),
                            });
                            continue;
                        }
                    }
                } else {
                    // Riding along: one beside each replica's node, the link stays local.
                    nodes.iter().map(|(n, _)| (n.clone(), 1)).collect()
                };
                for (node, count) in &targets {
                    want.insert(key(m, &c.id, &c.digest, node), (*count, m, c));
                    *pending.entry(node.clone()).or_default() += *count as usize;
                }
            }
        }
    }

    out.wanted = want.values().map(|(n, _, _)| *n as u64).sum();
    let (mut starts, mut stops) = (Vec::new(), Vec::new());
    let all: BTreeSet<&Key> = want.keys().chain(have.keys()).collect();
    let mut still_surplus = BTreeMap::new();

    // Absolute counts, not deltas: a delta re-derived before the heartbeat was issued twice.
    for k in all {
        let w = want.get(k).map(|(n, _, _)| *n).unwrap_or(0);
        let h = have.get(k).copied().unwrap_or(0);
        if w == h {
            continue;
        }

        // Scaling DOWN (even to zero) waits for the surplus to persist; scaling up does not.
        if w < h {
            // Never reset on emit: a landed command prunes the entry, otherwise we re-emit.
            let passes = hyst.seen.get(k).copied().unwrap_or(0) + 1;
            still_surplus.insert(k.clone(), passes);
            if passes < cfg.settle_passes {
                continue;
            }
        }

        match want.get(k) {
            Some((_, m, c)) => starts.push(Command::Start {
                node: k.4.clone(),
                tenant: m.tenant.clone(),
                app: m.app.clone(),
                component: c.id.clone(),
                digest: c.digest.clone(),
                count: w,
                config: c.config.clone(),
                secrets: c.secrets.clone(),
                links: m.link_table(&c.id),
                host_needs: c.host_needs.clone(),
                egress: c.egress.clone(),
                ingress_host: m
                    .ingress
                    .as_ref()
                    .filter(|i| i.component == c.id)
                    .map(|i| i.host.clone()),
            }),
            None => stops.push(Command::Stop {
                node: k.4.clone(),
                tenant: k.0.clone(),
                app: k.1.clone(),
                component: k.2.clone(),
                digest: k.3.clone(),
                count: h,
            }),
        }
    }
    hyst.seen = still_surplus;

    // Starts first, so a digest change brings the replacement up before the old one goes.
    let total = starts.len() + stops.len();
    starts.extend(stops);
    if starts.len() > cfg.max_commands {
        starts.truncate(cfg.max_commands);
    }
    out.deferred = total - starts.len();
    out.commands = starts;
    out
}

/// The placement a converged component keeps without ranking (ADR-0056); declines when unsure.
fn settled(
    c: &Component,
    want_replicas: u32,
    running: u32,
    mine: Option<&BTreeMap<&str, u32>>,
    fitting: &impl Fn(&Component, &str) -> bool,
    // False after any node joins, leaves or relabels.
    fleet_stable: bool,
) -> Option<Vec<(String, u32)>> {
    if !fleet_stable || running == 0 || running != want_replicas || c.placement.mode == Mode::Daemon
    {
        // Daemon is one per eligible node, so a new node must grow it.
        return None;
    }
    let mine = mine?;
    // One per node is the only state where the lookup provably equals the ranking.
    if mine.len() as u32 != want_replicas {
        return None;
    }
    if !mine.keys().all(|n| fitting(c, n)) {
        return None;
    }
    Some(mine.iter().map(|(n, count)| (n.to_string(), *count)).collect())
}

/// From `host_needs` (the real WIT surface), not a `stateful:` flag that could disagree.
// ponytail: any keyvalue import counts; the safe reading of an ambiguity is the one that refuses.
fn holds_state(m: &Manifest) -> bool {
    m.components
        .iter()
        .flat_map(|c| c.host_needs.iter())
        .any(|h| h.starts_with("wasi:keyvalue/") || h.starts_with("wasi:blobstore/"))
}

fn key(m: &Manifest, component: &str, digest: &str, node: &str) -> Key {
    (m.tenant.clone(), m.app.clone(), component.into(), digest.into(), node.into())
}

/// Largest remainder in integers: the total stays exact and independent of summation order.
fn proportional(total: u32, weights: &[usize]) -> Vec<u32> {
    // A zero weight would lose replicas, so it reads as the smallest share.
    let weights: Vec<usize> = weights.iter().map(|w| (*w).max(1)).collect();
    let sum: usize = weights.iter().sum::<usize>().max(1);
    let mut out: Vec<u32> = Vec::with_capacity(weights.len());
    let mut remainders: Vec<(usize, usize)> = Vec::with_capacity(weights.len());
    let mut given = 0u32;
    for (i, w) in weights.iter().enumerate() {
        let exact = total as usize * w;
        let whole = (exact / sum) as u32;
        out.push(whole);
        given += whole;
        remainders.push((exact % sum, i));
    }
    // Ties go to the earlier, already ranked, node.
    remainders.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    for (_, i) in remainders.into_iter().take((total - given) as usize) {
        out[i] += 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn place<'a>(
    c: &'a Component,
    // `(tenant, app)`: without it the ranking would match another tenant's component.
    owner: (&str, &str),
    // With autoscaling the count is a function of load; `c` is only the policy.
    replicas: u32,
    observed: &'a [NodeInventory],
    running_by_node: &BTreeMap<Owner<'_>, BTreeMap<&str, u32>>,
    node_load: &BTreeMap<&str, usize>,
    eligible_for: &mut EligibleFor<'a>,
    // Without it every app ranks against the same stale inventory and lands on one node.
    pending: &BTreeMap<String, usize>,
) -> Result<Vec<(String, u32)>, String> {
    // Apps wanting the same thing pay for one scan.
    let eligible: &Vec<&NodeInventory> = eligible_for
        .entry((&c.placement.constraints, &c.host_needs))
        .or_insert_with(|| observed.iter().filter(|n| fits(c, n)).collect());

    match c.placement.mode {
        Mode::Pinned => {
            let missing: Vec<&str> = c
                .placement
                .nodes
                .iter()
                .filter(|want| !eligible.iter().any(|n| n.node == **want))
                .map(|s| s.as_str())
                .collect();
            if !missing.is_empty() {
                return Err(format!(
                    "pinned to {missing:?}, which {} not available or do not meet the component's constraints",
                    if missing.len() == 1 { "is" } else { "are" }
                ));
            }
            Ok(c.placement.nodes.iter().map(|n| (n.clone(), 1)).collect())
        }
        _ if eligible.is_empty() => Err(format!(
            "no node satisfies constraints {:?} and host interfaces {:?}",
            c.placement.constraints, c.host_needs
        )),
        Mode::Daemon => Ok(eligible.iter().map(|n| (n.node.clone(), 1)).collect()),
        Mode::Spread => {
            if replicas == 0 {
                return Ok(Vec::new());
            }
            // Keys: this component's replicas here (desc: stability), total instances (asc:
            // balance across apps), name (determinism).
            let mine = running_by_node.get(&(owner.0, owner.1, c.id.as_str(), c.digest.as_str()));
            let mut ranked: Vec<(&NodeInventory, u32, usize)> = eligible
                .iter()
                .map(|n| {
                    let here = mine.and_then(|m| m.get(n.node.as_str())).copied().unwrap_or(0);
                    let observed_load = node_load.get(n.node.as_str()).copied().unwrap_or(0);
                    (*n, here, observed_load + pending.get(&n.node).copied().unwrap_or(0))
                })
                .collect();
            // Key 2 is load per CPU, cross-multiplied to stay integer and never divide by zero.
            let rank = |a: &(&NodeInventory, u32, usize), b: &(&NodeInventory, u32, usize)| {
                let (aw, bw) = (a.0.capacity.weight(), b.0.capacity.weight());
                b.1.cmp(&a.1).then((a.2 * bw).cmp(&(b.2 * aw))).then(a.0.node.cmp(&b.0.node))
            };
            // Only the first `replicas` are read: partition then sort the prefix, O(nodes) per app.
            let k = replicas as usize;
            if k < ranked.len() {
                ranked.select_nth_unstable_by(k, rank);
                ranked[..k].sort_by(rank);
            } else {
                ranked.sort_by(rank);
            }

            // Proportional to capacity: 3 of 4 replicas on a 10-core box, 1 on a 4-core (ADR-0039).
            let weights: Vec<usize> = ranked.iter().map(|(n, _, _)| n.capacity.weight()).collect();
            let shares: Vec<u32> = if (replicas as usize) <= ranked.len() {
                // Fewer replicas than nodes: one each, in ranking order.
                (0..ranked.len()).map(|i| u32::from((i as u32) < replicas)).collect()
            } else {
                proportional(replicas, &weights)
            };
            Ok(ranked
                .iter()
                .zip(shares)
                .map(|((node, _, _), count)| (node.node.clone(), count))
                .filter(|(_, count)| *count > 0)
                .collect())
        }
    }
}

fn fits(c: &Component, n: &NodeInventory) -> bool {
    c.placement.constraints.iter().all(|(k, v)| n.labels.get(k) == Some(v))
        && c.host_needs.iter().all(|need| n.host_ifaces.contains(need))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sized(name: &str, cpus: usize) -> NodeInventory {
        let mut n = node(name, &[], &[]);
        n.capacity = Capacity { cpus };
        n
    }

    fn node(name: &str, labels: &[(&str, &str)], ifaces: &[&str]) -> NodeInventory {
        NodeInventory {
            capacity: Capacity::default(),
            node: name.into(),
            labels: labels.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
            host_ifaces: ifaces.iter().map(|s| s.to_string()).collect(),
            kv_shared: false,
            address: String::new(),
            instances: Vec::new(),
        }
    }

    /// A node whose store every replica can see (`--kv nats`).
    fn shared(name: &str) -> NodeInventory {
        NodeInventory { kv_shared: true, ..node(name, &[], &[]) }
    }

    /// A component that keeps state, so the split-brain check applies to it.
    fn stateful(id: &str, digest: &str, replicas: u32) -> Component {
        Component { host_needs: vec!["wasi:keyvalue/store".into()], ..comp(id, digest, replicas) }
    }

    fn running(inv: &mut NodeInventory, component: &str, digest: &str, count: u32) {
        inv.instances.push(RunningInstance {
            tenant: "alice".into(),
            app: "mesh".into(),
            component: component.into(),
            digest: digest.into(),
            count,
            ingress_host: None,
        });
    }

    fn comp(id: &str, digest: &str, replicas: u32) -> Component {
        Component {
            id: id.into(),
            digest: digest.into(),
            replicas,
            scale: None,
            placement: Placement::default(),
            host_needs: Vec::new(),
            config: BTreeMap::new(),
            secrets: Vec::new(),
            egress: Vec::new(),
        }
    }

    fn app(components: Vec<Component>, links: Vec<Link>, strategy: Strategy) -> Manifest {
        let root = components[0].id.clone();
        Manifest {
            app: "mesh".into(),
            tenant: "alice".into(),
            strategy,
            components,
            links,
            ingress: Some(Ingress { host: "mesh.example.com".into(), component: root }),
        }
    }

    /// Run to convergence, applying commands to the inventory the way a host would.
    fn converge(desired: &[Manifest], observed: &mut [NodeInventory], passes: u32) -> Outcome {
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();
        let mut last = Outcome::default();
        for _ in 0..passes {
            last = plan(desired, observed, None, &mut hyst, &cfg);
            for cmd in &last.commands {
                apply(observed, cmd);
            }
        }
        last
    }

    fn apply(observed: &mut [NodeInventory], cmd: &Command) {
        match cmd {
            Command::Start { node, tenant, app, component, digest, count, .. } => {
                let inv = observed.iter_mut().find(|n| n.node == *node).expect("node");
                // The whole owner, like the host's `<tenant>/<app>/<component>` instance id.
                match inv.instances.iter_mut().find(|i| {
                    i.tenant == *tenant
                        && i.app == *app
                        && i.component == *component
                        && i.digest == *digest
                }) {
                    // Absolute, not additive.
                    Some(i) => i.count = *count,
                    None => inv.instances.push(RunningInstance {
                        tenant: tenant.clone(),
                        app: app.clone(),
                        component: component.clone(),
                        digest: digest.clone(),
                        count: *count,
                        ingress_host: None,
                    }),
                }
            }
            Command::Stop { node, tenant, app, component, digest, .. } => {
                let inv = observed.iter_mut().find(|n| n.node == *node).expect("node");
                inv.instances.retain(|i| {
                    !(i.tenant == *tenant
                        && i.app == *app
                        && i.component == *component
                        && i.digest == *digest)
                });
            }
        }
    }

    fn counts(observed: &[NodeInventory], component: &str) -> Vec<(String, u32)> {
        observed
            .iter()
            .map(|n| {
                (
                    n.node.clone(),
                    n.instances.iter().filter(|i| i.component == component).map(|i| i.count).sum(),
                )
            })
            .filter(|(_, c)| *c > 0)
            .collect()
    }

    #[test]
    fn a_deficit_is_filled_on_the_first_pass() {
        // Under-replicated must never wait for hysteresis.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 2, "{:?}", out.commands);
        assert!(out.commands.iter().all(|c| matches!(c, Command::Start { count: 1, .. })));
    }

    #[test]
    fn a_surplus_waits_for_two_consecutive_passes() {
        let m = app(vec![comp("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let mut a = node("box-a", &[], &[]);
        running(&mut a, "api", "sha256:a", 3);
        let obs = vec![a];
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();

        let first = plan(std::slice::from_ref(&m), &obs, None, &mut hyst, &cfg);
        assert!(first.commands.is_empty(), "must not stop on the first sighting");
        // Absolute: "hold 1", not "drop 2". Re-sending it is a no-op.
        let second = plan(&[m], &obs, None, &mut hyst, &cfg);
        assert!(
            matches!(&second.commands[..], [Command::Start { count: 1, node, .. }] if node == "box-a"),
            "{:?}",
            second.commands
        );
    }

    #[test]
    fn a_surplus_that_goes_away_resets_the_counter() {
        // The flap case: surplus, then not, then surplus again must not add up to a stop.
        let m = app(vec![comp("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let mut over = node("box-a", &[], &[]);
        running(&mut over, "api", "sha256:a", 2);
        let mut exact = node("box-a", &[], &[]);
        running(&mut exact, "api", "sha256:a", 1);
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();

        assert!(plan(std::slice::from_ref(&m), &[over.clone()], None, &mut hyst, &cfg)
            .commands
            .is_empty());
        assert!(plan(std::slice::from_ref(&m), &[exact], None, &mut hyst, &cfg)
            .commands
            .is_empty());
        assert!(
            plan(&[m], &[over], None, &mut hyst, &cfg).commands.is_empty(),
            "the counter must have restarted, not carried over"
        );
    }

    #[test]
    fn a_vanished_node_is_replaced_without_any_delete_handling() {
        // A silent node simply stops appearing.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let mut obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        converge(std::slice::from_ref(&m), &mut obs, 2);
        assert_eq!(counts(&obs, "api"), vec![("box-a".into(), 1), ("box-b".into(), 1)]);

        obs.retain(|n| n.node != "box-b");
        converge(&[m], &mut obs, 2);
        assert_eq!(
            counts(&obs, "api"),
            vec![("box-a".into(), 2)],
            "both replicas land on the survivor"
        );
    }

    #[test]
    fn a_joining_node_does_not_shuffle_existing_replicas() {
        // Spread has to be stable or every node join is a fleet-wide restart.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let mut obs = vec![node("box-a", &[], &[])];
        converge(std::slice::from_ref(&m), &mut obs, 2);
        assert_eq!(counts(&obs, "api"), vec![("box-a".into(), 2)]);

        obs.push(node("box-b", &[], &[]));
        let out = converge(&[m], &mut obs, 3);
        assert!(out.commands.is_empty(), "settled: {:?}", out.commands);
        // One moves, one stays; not a stop-then-start of each.
        assert_eq!(counts(&obs, "api"), vec![("box-a".into(), 1), ("box-b".into(), 1)]);
    }

    #[test]
    fn constraints_and_host_needs_both_gate_placement() {
        let mut c = comp("api", "sha256:a", 1);
        c.placement.constraints.insert("region".into(), "eu-central".into());
        c.host_needs.push("wasi:keyvalue/store@0.2.0-draft".into());
        let m = app(vec![c], vec![], Strategy::Linked);

        let obs = vec![
            node("wrong-region", &[("region", "us-east")], &["wasi:keyvalue/store@0.2.0-draft"]),
            node("no-kv", &[("region", "eu-central")], &[]),
            node("good", &[("region", "eu-central")], &["wasi:keyvalue/store@0.2.0-draft"]),
        ];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 1);
        assert_eq!(out.commands[0].node(), "good");
    }

    #[test]
    fn nothing_eligible_is_reported_not_silently_dropped() {
        let mut c = comp("api", "sha256:a", 1);
        c.placement.constraints.insert("region".into(), "antarctica".into());
        let m = app(vec![c], vec![], Strategy::Linked);
        let out = plan(
            &[m],
            &[node("box-a", &[], &[])],
            None,
            &mut Hysteresis::default(),
            &Cfg::default(),
        );
        assert!(out.commands.is_empty());
        assert_eq!(out.unschedulable.len(), 1);
        assert!(out.unschedulable[0].reason.contains("antarctica"), "{:?}", out.unschedulable);
    }

    #[test]
    fn a_pinned_node_that_is_gone_is_unschedulable_not_relocated() {
        // Pinned means pinned: placing it elsewhere would defeat the pin (a GPU, a jurisdiction).
        let mut c = comp("api", "sha256:a", 1);
        c.placement.mode = Mode::Pinned;
        c.placement.nodes = vec!["box-gpu".into()];
        let m = app(vec![c], vec![], Strategy::Linked);
        let out = plan(
            &[m],
            &[node("box-a", &[], &[])],
            None,
            &mut Hysteresis::default(),
            &Cfg::default(),
        );
        assert!(out.commands.is_empty());
        assert!(out.unschedulable[0].reason.contains("box-gpu"), "{:?}", out.unschedulable);
    }

    #[test]
    fn daemon_puts_one_on_every_eligible_node_and_ignores_replicas() {
        let mut c = comp("api", "sha256:a", 7);
        c.placement.mode = Mode::Daemon;
        let m = app(vec![c], vec![], Strategy::Linked);
        let mut obs =
            vec![node("box-a", &[], &[]), node("box-b", &[], &[]), node("box-c", &[], &[])];
        converge(&[m], &mut obs, 2);
        assert_eq!(
            counts(&obs, "api"),
            vec![("box-a".into(), 1), ("box-b".into(), 1), ("box-c".into(), 1)]
        );
    }

    #[test]
    fn a_linked_graph_co_locates_and_every_link_resolves_locally() {
        let m = app(
            vec![comp("api", "sha256:a", 2), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let mut obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        converge(&[m], &mut obs, 2);

        for n in &obs {
            let ids: BTreeSet<&str> = n.instances.iter().map(|i| i.component.as_str()).collect();
            assert!(ids.contains("api") && ids.contains("store"), "{}: {ids:?}", n.node);
        }
    }

    #[test]
    fn a_start_carries_the_link_table_the_host_binds_against() {
        let m = app(
            vec![comp("api", "sha256:a", 1), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let out = plan(
            &[m],
            &[node("box-a", &[], &[])],
            None,
            &mut Hysteresis::default(),
            &Cfg::default(),
        );
        let api = out
            .commands
            .iter()
            .find(|c| matches!(c, Command::Start { component, .. } if component == "api"))
            .expect("api start");
        let Command::Start { links, .. } = api else { unreachable!() };
        assert_eq!(links["records:store/store@0.1.0"], "alice/mesh/store");

        let store = out
            .commands
            .iter()
            .find(|c| matches!(c, Command::Start { component, .. } if component == "store"))
            .expect("store start");
        let Command::Start { links, .. } = store else { unreachable!() };
        assert!(links.is_empty());
    }

    #[test]
    fn fused_starts_only_the_root() {
        // `wac plug` erased the parts at build time; the manifest keeps them as the build recipe only.
        let m = app(
            vec![comp("api", "sha256:fused", 1), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Fused,
        );
        let out = plan(
            &[m],
            &[node("box-a", &[], &[])],
            None,
            &mut Hysteresis::default(),
            &Cfg::default(),
        );
        assert_eq!(out.commands.len(), 1);
        assert!(matches!(&out.commands[0], Command::Start { component, .. } if component == "api"));
    }

    #[test]
    fn split_strategy_decomposes_components_without_code_changes() {
        let json = r#"{"strategy":"split"}"#;
        #[derive(Deserialize)]
        struct S {
            strategy: Strategy,
        }
        let s: S = serde_json::from_str(json).expect("split deserializes as Strategy");
        assert_eq!(s.strategy, Strategy::Linked);

        // Switching from fused to split schedules both components with zero code change in the components
        let m = app(
            vec![comp("api", "sha256:same_bytecode", 1), comp("store", "sha256:same_bytecode", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            s.strategy,
        );
        let out = plan(
            &[m],
            &[node("box-a", &[], &[])],
            None,
            &mut Hysteresis::default(),
            &Cfg::default(),
        );
        assert_eq!(out.commands.len(), 2);
    }

    #[test]
    fn a_new_digest_starts_before_the_old_one_stops() {
        let mut a = node("box-a", &[], &[]);
        running(&mut a, "api", "sha256:old", 1);
        let m = app(vec![comp("api", "sha256:new", 1)], vec![], Strategy::Linked);
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();

        let first = plan(std::slice::from_ref(&m), &[a.clone()], None, &mut hyst, &cfg);
        assert_eq!(first.commands.len(), 1, "the new one comes up alone first");
        assert!(
            matches!(&first.commands[0], Command::Start { digest, .. } if digest == "sha256:new")
        );

        let second = plan(&[m], &[a], None, &mut hyst, &cfg);
        assert!(second
            .commands
            .iter()
            .any(|c| matches!(c, Command::Stop { digest, .. } if digest == "sha256:old")));
        // Ordering holds within a pass too.
        let kinds: Vec<bool> =
            second.commands.iter().map(|c| matches!(c, Command::Start { .. })).collect();
        let first_stop = kinds.iter().position(|s| !s).unwrap_or(kinds.len());
        assert!(kinds[..first_stop].iter().all(|s| *s), "starts must precede stops");
    }

    #[test]
    fn an_app_that_leaves_the_desired_set_is_stopped() {
        let mut a = node("box-a", &[], &[]);
        running(&mut a, "api", "sha256:a", 1);
        let cfg = Cfg::default();
        let mut hyst = Hysteresis::default();
        plan(&[], &[a.clone()], None, &mut hyst, &cfg);
        let out = plan(&[], &[a], None, &mut hyst, &cfg);
        assert!(matches!(&out.commands[0], Command::Stop { app, .. } if app == "mesh"));
    }

    #[test]
    fn the_command_cap_defers_rather_than_drops() {
        let m = app(vec![comp("api", "sha256:a", 50)], vec![], Strategy::Linked);
        let obs: Vec<NodeInventory> =
            (0..50).map(|i| node(&format!("box-{i:02}"), &[], &[])).collect();
        let cfg = Cfg { max_commands: 20, ..Cfg::default() };
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &cfg);
        assert_eq!(out.commands.len(), 20);
        assert_eq!(out.deferred, 30);
    }

    /// Two replicas on node-local stores silently split the rate limiter's state.
    #[test]
    fn a_stateful_app_is_refused_rather_than_split_across_local_stores() {
        let m = app(vec![stateful("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let obs = vec![
            node("box-a", &[], &["wasi:keyvalue/store"]),
            node("box-b", &[], &["wasi:keyvalue/store"]),
        ];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "nothing may be placed: {:?}", out.commands);
        let reason = &out.unschedulable[0].reason;
        assert!(reason.contains("diverge in silence"), "{reason}");
        // The reason has to name the offending nodes, or the operator cannot act.
        assert!(reason.contains("box-a") && reason.contains("box-b"), "{reason}");
    }

    #[test]
    fn the_same_app_places_fine_on_nodes_with_a_shared_store() {
        let m = app(vec![stateful("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let mut a = shared("box-a");
        let mut b = shared("box-b");
        a.host_ifaces = vec!["wasi:keyvalue/store".into()];
        b.host_ifaces = vec!["wasi:keyvalue/store".into()];
        let out = plan(&[m], &[a, b], None, &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 2, "{:?}", out.unschedulable);
        assert!(out.unschedulable.is_empty());
    }

    #[test]
    fn one_replica_on_a_local_store_is_fine() {
        // The single-node self-hosting lane (sqlite); refusing this would break it.
        let m = app(vec![stateful("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let obs = vec![
            node("box-a", &[], &["wasi:keyvalue/store"]),
            node("box-b", &[], &["wasi:keyvalue/store"]),
        ];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 1);
        assert!(out.unschedulable.is_empty());
    }

    #[test]
    fn a_stateless_app_spreads_freely_over_local_stores() {
        // No keyvalue import, nothing to diverge: the check must not become a blanket ban on spreading.
        let m = app(vec![comp("api", "sha256:a", 2)], vec![], Strategy::Linked);
        let obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.commands.len(), 2);
        assert!(out.unschedulable.is_empty());
    }

    #[test]
    fn a_plug_holding_state_counts_even_when_the_root_does_not() {
        // The graph is co-located, so a stateful PLUG lands on every node the root does.
        let m = app(
            vec![comp("api", "sha256:a", 2), stateful("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let obs = vec![
            node("box-a", &[], &["wasi:keyvalue/store"]),
            node("box-b", &[], &["wasi:keyvalue/store"]),
        ];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "{:?}", out.commands);
        assert!(out.unschedulable[0].reason.contains("diverge"));
    }

    #[test]
    fn an_unreported_kv_shared_reads_as_node_local() {
        // Fail closed: a node predating this field, or partly parsed, must not be treated as safe.
        let inv: NodeInventory = serde_json::from_str(
            r#"{"node":"old","host_ifaces":["wasi:keyvalue/store"],"instances":[]}"#,
        )
        .expect("parses");
        assert!(!inv.kv_shared, "an absent kv_shared must read as node-local");
    }

    #[test]
    fn a_plug_with_its_own_placement_lands_on_a_different_node() {
        // Lets a graph SPAN nodes; without it every component is pinned to the root's nodes.
        let mut plug = comp("store", "sha256:b", 1);
        plug.placement.constraints.insert("role".into(), "data".into());
        // The root is pinned to the web tier too, so the test asserts placement, not tie-break order.
        let mut root = comp("api", "sha256:a", 1);
        root.placement.constraints.insert("role".into(), "web".into());
        let m = app(
            vec![root, plug],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let obs =
            vec![node("edge", &[("role", "web")], &[]), node("data-1", &[("role", "data")], &[])];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        let where_ = |c: &str| {
            out.commands
                .iter()
                .find(|x| matches!(x, Command::Start { component, .. } if component == c))
                .map(|x| x.node().to_string())
        };
        assert_eq!(where_("api").as_deref(), Some("edge"));
        assert_eq!(
            where_("store").as_deref(),
            Some("data-1"),
            "the plug follows its own placement"
        );
        assert_ne!(where_("api"), where_("store"), "the graph must actually span");
    }

    #[test]
    fn a_plug_with_its_own_placement_gets_every_replica_it_asks_for() {
        let mut plug = comp("store", "sha256:b", 2);
        plug.placement.constraints.insert("role".into(), "data".into());
        let m = app(
            vec![comp("api", "sha256:a", 1), plug],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let obs = vec![node("edge", &[], &[]), node("data-1", &[("role", "data")], &[])];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        let store: u32 = out
            .commands
            .iter()
            .filter_map(|c| match c {
                Command::Start { component, count, .. } if component == "store" => Some(*count),
                _ => None,
            })
            .sum();
        assert_eq!(store, 2, "two asked for, one node fits: both go there");
        assert_eq!(out.wanted, 3);
    }

    #[test]
    fn a_plug_without_its_own_placement_still_rides_along() {
        // Co-location stays the default (faster, ADR-0019's 1.2ms); spanning is opt-in.
        let m = app(
            vec![comp("api", "sha256:a", 1), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let obs = vec![node("n1", &[], &[]), node("n2", &[], &[])];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        let nodes: std::collections::BTreeSet<&str> =
            out.commands.iter().map(|c| c.node()).collect();
        assert_eq!(nodes.len(), 1, "both parts on one node: {nodes:?}");
    }

    #[test]
    fn a_plug_that_cannot_be_placed_is_reported_against_its_own_name() {
        // "app unschedulable" must say which component, with ten in a graph.
        let mut plug = comp("store", "sha256:b", 1);
        plug.placement.constraints.insert("role".into(), "gpu".into());
        let m = app(vec![comp("api", "sha256:a", 1), plug], vec![], Strategy::Linked);
        let out =
            plan(&[m], &[node("n1", &[], &[])], None, &mut Hysteresis::default(), &Cfg::default());
        assert!(out.unschedulable[0].reason.starts_with("`store`:"), "{:?}", out.unschedulable);
    }

    #[test]
    fn different_apps_spread_across_nodes_instead_of_piling_onto_one() {
        // Every app is new, so key 1 ties; without key 2 all six land on the first node.
        let mut obs = vec![node("n1", &[], &[]), node("n2", &[], &[]), node("n3", &[], &[])];
        let apps: Vec<Manifest> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .map(|n| {
                let mut m = app(vec![comp(n, "sha256:x", 1)], vec![], Strategy::Linked);
                m.app = n.to_string();
                m
            })
            .collect();
        converge(&apps, &mut obs, 3);
        let per_node: Vec<usize> = obs.iter().map(|n| n.instances.len()).collect();
        assert_eq!(per_node.iter().sum::<usize>(), 6);
        assert!(
            per_node.iter().all(|c| *c == 2),
            "six apps over three nodes should be 2/2/2, got {per_node:?}"
        );
    }

    #[test]
    fn balancing_across_apps_does_not_break_stability_within_one() {
        // Key 1 still wins: an existing replica must not move because its node is busier.
        let m = app(vec![comp("api", "sha256:a", 1)], vec![], Strategy::Linked);
        let mut busy = node("busy", &[], &[]);
        running(&mut busy, "api", "sha256:a", 1);
        running(&mut busy, "other", "sha256:z", 5);
        let idle = node("idle", &[], &[]);
        let out = plan(&[m], &[busy, idle], None, &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "it must stay put: {:?}", out.commands);
    }

    #[test]
    fn a_settled_world_emits_nothing() {
        // Convergence is a fixed point: a healthy fleet is silent.
        let m = app(
            vec![comp("api", "sha256:a", 3), comp("store", "sha256:b", 1)],
            vec![Link {
                plug: "store".into(),
                socket: "api".into(),
                iface: "records:store/store@0.1.0".into(),
            }],
            Strategy::Linked,
        );
        let mut obs = vec![node("box-a", &[], &[]), node("box-b", &[], &[])];
        converge(std::slice::from_ref(&m), &mut obs, 3);
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "not settled: {:?}", out.commands);
        assert!(out.unschedulable.is_empty());
    }

    fn scaled(min: u32, max: u32, target: u32) -> Component {
        let mut c = comp("gate", "sha256:aa", 1);
        c.scale = Some(Scale { min, max, target });
        c
    }

    fn app_with(c: Component, host: &str) -> Manifest {
        let mut m = app(vec![c], vec![], Strategy::Fused);
        m.ingress = Some(Ingress { host: host.into(), component: "gate".into() });
        m
    }

    #[test]
    fn replicas_track_concurrency_between_min_and_max() {
        let m = app_with(scaled(1, 10, 10), "shop.eve.test");
        let mut load = Load::new();
        for (inflight, want) in [(0, 1), (1, 1), (10, 1), (11, 2), (25, 3), (100, 10), (500, 10)] {
            load.insert("shop.eve.test".into(), inflight);
            assert_eq!(
                desired_replicas(&m.components[0], &m, Some(&load), 0),
                want,
                "{inflight} in flight at target 10 should want {want}"
            );
        }
    }

    #[test]
    fn a_missing_sample_holds_the_count_instead_of_scaling_to_min() {
        // Ingress restart or first pass must not read as "no traffic" and scale a busy app to `min`.
        let mut c = scaled(0, 10, 10);
        c.replicas = 4;
        let m = app_with(c, "shop.eve.test");
        assert_eq!(
            desired_replicas(&m.components[0], &m, Some(&Load::new()), 4),
            4,
            "no sample must hold the current count, not collapse to min"
        );
    }

    #[test]
    fn an_app_with_no_scale_block_is_untouched_by_load() {
        // Every existing manifest is this case; load must be inert for them.
        let m = app_with(comp("gate", "sha256:aa", 3), "shop.eve.test");
        let load = Load::from([("shop.eve.test".to_string(), 900)]);
        assert_eq!(desired_replicas(&m.components[0], &m, Some(&load), 0), 3);
    }

    #[test]
    fn scale_to_zero_is_reachable_and_a_request_brings_it_back() {
        let m = app_with(scaled(0, 5, 10), "shop.eve.test");
        let idle = Load::from([("shop.eve.test".to_string(), 0)]);
        assert_eq!(
            desired_replicas(&m.components[0], &m, Some(&idle), 0),
            0,
            "idle scales to zero"
        );
        let one = Load::from([("shop.eve.test".to_string(), 1)]);
        assert_eq!(
            desired_replicas(&m.components[0], &m, Some(&one), 0),
            1,
            "one request brings it back"
        );
    }

    #[test]
    fn a_nonsense_scale_block_cannot_produce_a_nonsense_count() {
        // max below min and a target of zero must neither panic nor divide by zero.
        let m = app_with(scaled(3, 1, 0), "shop.eve.test");
        let load = Load::from([("shop.eve.test".to_string(), 7)]);
        let n = desired_replicas(&m.components[0], &m, Some(&load), 0);
        assert!(n >= 3, "min must still hold when max is below it, got {n}");
    }

    #[test]
    fn scaling_up_actually_places_the_new_replicas() {
        // desired_replicas is arithmetic; this checks the number reaches placement.
        let m = app_with(scaled(1, 4, 10), "shop.eve.test");
        let nodes = vec![node("n1", &[], &[]), node("n2", &[], &[])];
        let load = Load::from([("shop.eve.test".to_string(), 31)]);
        let mut h = Hysteresis::default();
        let out = plan(&[m], &nodes, Some(&load), &mut h, &Cfg::default());
        let total: u32 = out
            .commands
            .iter()
            .filter_map(|c| match c {
                Command::Start { count, .. } => Some(*count),
                _ => None,
            })
            .sum();
        assert_eq!(total, 4, "31 in flight at target 10 is 4 replicas: {:?}", out.commands);
    }

    #[test]
    fn a_proportional_split_is_exact_and_favours_the_bigger_node() {
        // ADR-0039: 4 replicas over 10 and 4 cores place 3 and 1, not 2 and 2.
        assert_eq!(proportional(4, &[10, 4]), vec![3, 1]);
        for total in 0..20u32 {
            for weights in [vec![10, 4], vec![1, 1, 1], vec![8, 4, 2, 1], vec![5]] {
                let got = proportional(total, &weights);
                assert_eq!(got.iter().sum::<u32>(), total, "{total} over {weights:?} -> {got:?}");
            }
        }
        assert_eq!(proportional(6, &[1, 1, 1]), vec![2, 2, 2], "equal nodes split evenly");
        assert_eq!(proportional(1, &[10, 4]), vec![1, 0], "one replica goes to the bigger");
        assert_eq!(proportional(3, &[0, 0]), vec![2, 1], "a node advertising nothing still counts");
    }

    #[test]
    fn spread_gives_the_bigger_machine_the_bigger_share() {
        let mut c = comp("gate", "sha256:aa", 4);
        c.placement.mode = Mode::Spread;
        let m = app(vec![c], vec![], Strategy::Fused);
        let obs = vec![sized("mac", 10), sized("pi", 4)];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        let mut got: Vec<(String, u32)> = out
            .commands
            .iter()
            .filter_map(|c| match c {
                Command::Start { node, count, .. } => Some((node.clone(), *count)),
                _ => None,
            })
            .collect();
        got.sort();
        assert_eq!(got, vec![("mac".to_string(), 3), ("pi".to_string(), 1)], "10:4 cores -> 3:1");
    }

    #[test]
    fn a_busy_big_node_still_loses_to_an_idle_small_one() {
        // Per-CPU load, not raw load: ten instances on ten cores is busier per core than two on four.
        let mut mac = sized("mac", 10);
        mac.instances = vec![RunningInstance {
            tenant: "t".into(),
            app: "other".into(),
            component: "x".into(),
            digest: "sha256:zz".into(),
            count: 10,
            ingress_host: None,
        }];
        let mut pi = sized("pi", 4);
        pi.instances = vec![RunningInstance {
            tenant: "t".into(),
            app: "other".into(),
            component: "x".into(),
            digest: "sha256:zz".into(),
            count: 2,
            ingress_host: None,
        }];
        let mut c = comp("gate", "sha256:aa", 1);
        c.placement.mode = Mode::Spread;
        let m = app(vec![c], vec![], Strategy::Fused);
        let out = plan(&[m], &[mac, pi], None, &mut Hysteresis::default(), &Cfg::default());
        let node = out.commands.iter().find_map(|c| match c {
            Command::Start { node, .. } => Some(node.clone()),
            _ => None,
        });
        assert_eq!(node.as_deref(), Some("pi"), "1.0 per core beats 0.5 per core");
    }

    #[test]
    fn a_node_that_advertises_no_capacity_is_still_placeable() {
        // A host publishing no capacity must not vanish from placement or divide by zero.
        let old = node("old", &[], &[]);
        assert_eq!(old.capacity.weight(), 1);
        let mut c = comp("gate", "sha256:aa", 2);
        c.placement.mode = Mode::Spread;
        let m = app(vec![c], vec![], Strategy::Fused);
        let out = plan(&[m], &[old], None, &mut Hysteresis::default(), &Cfg::default());
        assert!(out.unschedulable.is_empty(), "{:?}", out.unschedulable);
        assert!(!out.commands.is_empty(), "a node with no advertised capacity must still be used");
    }

    #[test]
    fn losing_the_signal_holds_the_fleet_instead_of_shrinking_it() {
        // An app scaled to 6 that loses its load signal must not collapse to the manifest's `replicas`.
        let m = app_with(scaled(1, 10, 10), "shop.eve.test");
        assert_eq!(desired_replicas(&m.components[0], &m, None, 6), 6, "hold what runs");
        assert_eq!(desired_replicas(&m.components[0], &m, None, 0), 1, "but never below min");
        // A host with no sample yet is the same case.
        let empty = Load::new();
        assert_eq!(desired_replicas(&m.components[0], &m, Some(&empty), 6), 6);
    }

    #[test]
    fn hitting_the_ceiling_is_reported_rather_than_silently_clamped() {
        // `max` is the operator's limit, but a fleet pinned there with demand must not look correctly sized.
        let m = app_with(scaled(1, 2, 10), "shop.eve.test");
        let load = Load::from([("shop.eve.test".to_string(), 500)]);
        let obs = vec![node("n1", &[], &[]), node("n2", &[], &[])];
        let out = plan(&[m], &obs, Some(&load), &mut Hysteresis::default(), &Cfg::default());
        assert_eq!(out.at_ceiling.len(), 1, "{:?}", out.at_ceiling);
        let c = &out.at_ceiling[0];
        assert_eq!((c.max, c.wanted), (2, 50), "clamped to 2, demand asked for 50");
    }

    #[test]
    fn an_app_within_its_ceiling_reports_nothing() {
        let m = app_with(scaled(1, 10, 10), "shop.eve.test");
        let load = Load::from([("shop.eve.test".to_string(), 25)]);
        let obs = vec![node("n1", &[], &[])];
        let out = plan(&[m], &obs, Some(&load), &mut Hysteresis::default(), &Cfg::default());
        assert!(out.at_ceiling.is_empty(), "{:?}", out.at_ceiling);
    }

    #[test]
    fn two_providers_of_one_interface_are_refused_rather_than_one_winning() {
        // Keyed by interface, the second link would silently overwrite the first.
        let root = comp("gate", "sha256:aa", 1);
        let a = comp("store-a", "sha256:bb", 1);
        let b = comp("store-b", "sha256:cc", 1);
        let m = app(
            vec![root, a, b],
            vec![
                Link {
                    plug: "store-a".into(),
                    socket: "gate".into(),
                    iface: "records:store/store@0.1.0".into(),
                },
                Link {
                    plug: "store-b".into(),
                    socket: "gate".into(),
                    iface: "records:store/store@0.1.0".into(),
                },
            ],
            Strategy::Linked,
        );
        let obs = vec![node("n1", &[], &[])];
        let out = plan(&[m], &obs, None, &mut Hysteresis::default(), &Cfg::default());
        assert!(out.commands.is_empty(), "nothing may be placed: {:?}", out.commands);
        assert_eq!(out.unschedulable.len(), 1);
        let why = &out.unschedulable[0].reason;
        for expected in ["gate", "records:store/store@0.1.0", "store-a", "store-b"] {
            assert!(why.contains(expected), "{expected:?} missing from {why:?}");
        }
    }

    #[test]
    fn the_same_interface_for_different_components_is_fine() {
        // Two components importing one interface from their own providers is normal.
        let m = app(
            vec![
                comp("gate", "sha256:aa", 1),
                comp("api", "sha256:dd", 1),
                comp("store-a", "sha256:bb", 1),
                comp("store-b", "sha256:cc", 1),
            ],
            vec![
                Link {
                    plug: "store-a".into(),
                    socket: "gate".into(),
                    iface: "records:store/store@0.1.0".into(),
                },
                Link {
                    plug: "store-b".into(),
                    socket: "api".into(),
                    iface: "records:store/store@0.1.0".into(),
                },
            ],
            Strategy::Linked,
        );
        assert!(m.conflicting_links().is_empty(), "{:?}", m.conflicting_links());
    }

    #[test]
    fn one_component_may_import_several_different_interfaces() {
        let m = app(
            vec![comp("gate", "sha256:aa", 1), comp("store", "sha256:bb", 1)],
            vec![
                Link {
                    plug: "store".into(),
                    socket: "gate".into(),
                    iface: "records:store/store@0.1.0".into(),
                },
                Link {
                    plug: "store".into(),
                    socket: "gate".into(),
                    iface: "records:store/query@0.1.0".into(),
                },
            ],
            Strategy::Linked,
        );
        assert!(m.conflicting_links().is_empty(), "{:?}", m.conflicting_links());
    }

    /// A lookup that differs from the ranking would be a slow drift no single scenario catches.
    #[test]
    fn the_fast_path_never_places_differently_from_the_full_ranking() {
        let mut checked = 0;
        for nodes in 1..=5usize {
            for replicas in 1..=7u32 {
                for lopsided in [false, true] {
                    for concentrated in [false, true] {
                        let m =
                            app(vec![comp("api", "sha256:a", replicas)], vec![], Strategy::Linked);
                        let mut obs: Vec<NodeInventory> = (0..nodes)
                            .map(|i| {
                                let cpus = if lopsided && i == 0 { 16 } else { 4 };
                                sized(&format!("n{i}"), cpus)
                            })
                            .collect();
                        if concentrated {
                            // Everything on one node, as if the rest joined later.
                            obs[0].instances.push(RunningInstance {
                                tenant: m.tenant.clone(),
                                app: m.app.clone(),
                                component: "api".into(),
                                digest: "sha256:a".into(),
                                count: replicas,
                                ingress_host: None,
                            });
                        }

                        let run = |fast: bool| {
                            let cfg = Cfg { fast_path: fast, ..Cfg::default() };
                            let mut o = obs.clone();
                            let mut h = Hysteresis::default();
                            // Several passes: the fast path only engages once a pass has converged.
                            for _ in 0..6 {
                                let out = plan(std::slice::from_ref(&m), &o, None, &mut h, &cfg);
                                for cmd in &out.commands {
                                    apply(&mut o, cmd);
                                }
                            }
                            counts(&o, "api")
                        };

                        assert_eq!(
                            run(true),
                            run(false),
                            "nodes={nodes} replicas={replicas} lopsided={lopsided} \
                             concentrated={concentrated}"
                        );
                        checked += 1;
                    }
                }
            }
        }
        assert_eq!(checked, 140, "the sweep shrank — it is the whole guarantee");
    }

    /// Only the fleet under the app changed, so the fast path must not take it.
    #[test]
    fn a_node_that_stops_qualifying_gives_its_replica_up() {
        let mut c = comp("api", "sha256:a", 1);
        c.placement.constraints.insert("region".into(), "eu".into());
        let m = app(vec![c], vec![], Strategy::Linked);

        let mut obs =
            vec![node("box-a", &[("region", "eu")], &[]), node("box-b", &[("region", "eu")], &[])];
        converge(std::slice::from_ref(&m), &mut obs, 3);
        let placed = counts(&obs, "api");
        assert_eq!(placed.len(), 1, "one replica on one node: {placed:?}");
        let held = placed[0].0.clone();

        // The node it landed on is relabelled out of the region.
        obs.iter_mut()
            .find(|n| n.node == held)
            .unwrap()
            .labels
            .insert("region".into(), "us".into());
        converge(&[m], &mut obs, 4);

        let after = counts(&obs, "api");
        assert_eq!(after.len(), 1, "still exactly one replica: {after:?}");
        assert_ne!(after[0].0, held, "it must leave the node that no longer qualifies");
    }

    /// Counting by component id and digest alone let bob's replica satisfy alice's manifest.
    #[test]
    fn one_tenants_replicas_do_not_count_as_anothers() {
        let shared = "sha256:popular";
        let alice = Manifest {
            tenant: "alice".into(),
            app: "shop".into(),
            ..app(vec![comp("gate", shared, 1)], vec![], Strategy::Fused)
        };
        let bob = Manifest {
            tenant: "bob".into(),
            app: "shop".into(),
            ingress: Some(Ingress {
                host: "shop.bob.example.com".into(),
                component: "gate".into(),
            }),
            ..app(vec![comp("gate", shared, 1)], vec![], Strategy::Fused)
        };

        let mut observed = vec![node("n1", &[], &[])];
        converge(&[alice, bob], &mut observed, 4);

        let owners: BTreeSet<&str> =
            observed[0].instances.iter().map(|i| i.tenant.as_str()).collect();
        assert_eq!(
            owners,
            BTreeSet::from(["alice", "bob"]),
            "both tenants must get their own replica of the shared component, got {:?}",
            observed[0].instances
        );
    }
}
