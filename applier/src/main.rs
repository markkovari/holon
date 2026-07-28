//! `applier` — the platform's only holder of a Kubernetes credential.
//!
//! `platform-domain` (wasm) decides everything and renders the manifests; this
//! applies them. It exists for one reason: a wasm component cannot talk to the API
//! server, because `wasi:http` validates TLS against webpki roots and the API
//! server presents a cluster-CA certificate. See docs/adr/0003.
//!
//! It holds no business logic, no database and no user concept, so it stays small
//! enough to audit in one sitting — which matters, because it is the process with
//! the dangerous permission.
//!
//! **It does not trust its caller.** Every request names a namespace, and every
//! object in the payload must belong to that namespace, be of an allow-listed
//! kind, and carry no field we have not seen work. A bug on the wasm side
//! therefore cannot become a cross-tenant write.
//!
//! Modes:
//!   --validate-only   never builds a client; validates and reports (CI, tests)
//!   --dry-run         applies with dryRun=All against a real cluster
//!   (default)         server-side apply, field manager `platform`

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use clap::Parser;
use kube::api::{Api, DynamicObject, GroupVersionKind, Patch, PatchParams};
use kube::core::ApiResource;
use kube::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Kinds the platform is allowed to create. Anything else is refused, however it
/// got into the payload.
const ALLOWED_KINDS: &[(&str, &str)] = &[
    ("runtime.wasmcloud.dev/v1alpha1", "WorkloadDeployment"),
    ("v1", "Service"),
    ("v1", "Namespace"),
    ("v1", "ResourceQuota"),
    ("networking.k8s.io/v1", "NetworkPolicy"),
];

/// Fields we have not seen work on this cluster (used once anywhere, or only in a
/// comment). The renderer never emits them; this makes that a hard boundary rather
/// than a convention. See docs/adr/0003 and 0010.
const FORBIDDEN_KEYS: &[&str] = &["hostSelector", "configFrom", "secretFrom", "tun"];

#[derive(Parser, Clone)]
#[command(name = "applier", about = "Applies platform-rendered manifests to Kubernetes")]
struct Args {
    /// Listen address for the apply API.
    #[arg(long, default_value = "127.0.0.1:8088")]
    addr: String,

    /// Shared secret the caller must present as `x-platform-secret`.
    #[arg(long, env = "APPLIER_SECRET")]
    secret: String,

    /// Validate and report without building a Kubernetes client at all.
    #[arg(long)]
    validate_only: bool,

    /// Apply with dryRun=All (needs a cluster, changes nothing).
    #[arg(long)]
    dry_run: bool,

    /// Poll this platform for current revisions and re-apply them. Omit to disable.
    /// This is ADR-0004's drift correction: the platform has no scheduler, so the
    /// applier pulls.
    #[arg(long)]
    platform_url: Option<String>,

    /// Seconds between re-apply passes.
    #[arg(long, default_value = "300")]
    reapply_interval: u64,

    /// Only ever touch namespaces with this prefix. A second belt on top of the
    /// per-request namespace check.
    #[arg(long, default_value = "tenant-")]
    namespace_prefix: String,
}

#[derive(Deserialize)]
struct ApplyRequest {
    namespace: String,
    /// One or more YAML documents, as the renderer emitted them.
    manifests: String,
}

#[derive(Serialize)]
struct ApplyReport {
    namespace: String,
    applied: Vec<String>,
    dry_run: bool,
    validated_only: bool,
}

struct AppState {
    args: Args,
    client: Option<Client>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    if args.secret.trim().is_empty() {
        bail!("--secret must not be empty: it is the only thing standing between this process's credential and the network");
    }

    let client = if args.validate_only {
        eprintln!("applier: validate-only — no Kubernetes client will be built");
        None
    } else {
        Some(Client::try_default().await.context("building a Kubernetes client (is a kubeconfig or a ServiceAccount present?)")?)
    };

    let state = Arc::new(AppState { args: args.clone(), client });

    if let Some(url) = args.platform_url.clone() {
        let bg = state.clone();
        tokio::spawn(async move { reapply_loop(bg, url).await });
    }

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/apply", post(apply_handler))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&args.addr).await?;
    eprintln!(
        "applier: listening on http://{} | mode = {} | namespace prefix = {:?}",
        args.addr,
        if args.validate_only {
            "validate-only"
        } else if args.dry_run {
            "dry-run"
        } else {
            "apply"
        },
        args.namespace_prefix
    );
    axum::serve(listener, app).await?;
    Ok(())
}

fn authorized(headers: &HeaderMap, want: &str) -> bool {
    headers
        .get("x-platform-secret")
        .and_then(|v| v.to_str().ok())
        .map(|got| got == want)
        .unwrap_or(false)
}

async fn apply_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(req): Json<ApplyRequest>,
) -> impl IntoResponse {
    if !authorized(&headers, &state.args.secret) {
        return (StatusCode::UNAUTHORIZED, Json(json!({ "error": "bad or missing x-platform-secret" }))).into_response();
    }
    match apply(&state, &req).await {
        Ok(report) => (StatusCode::OK, Json(json!(report))).into_response(),
        Err(e) => (
            StatusCode::UNPROCESSABLE_ENTITY,
            Json(json!({ "error": "rejected", "detail": format!("{e:#}") })),
        )
            .into_response(),
    }
}

/// Parse, validate, then (unless validate-only) server-side apply.
async fn apply(state: &AppState, req: &ApplyRequest) -> Result<ApplyReport> {
    let ns = req.namespace.trim();
    if !ns.starts_with(&state.args.namespace_prefix) {
        bail!(
            "namespace {ns:?} does not start with {:?} — the applier only writes platform-managed namespaces",
            state.args.namespace_prefix
        );
    }
    if !ns.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
        bail!("namespace {ns:?} is not a DNS label");
    }

    let objects = parse_objects(&req.manifests)?;
    if objects.is_empty() {
        bail!("no objects in the payload");
    }

    let mut names = Vec::new();
    for obj in &objects {
        validate(obj, ns)?;
        names.push(describe(obj));
    }

    if state.args.validate_only {
        return Ok(ApplyReport {
            namespace: ns.to_string(),
            applied: names,
            dry_run: false,
            validated_only: true,
        });
    }

    let client = state.client.as_ref().expect("client present unless validate-only");
    let mut params = PatchParams::apply("platform").force();
    if state.args.dry_run {
        params = params.dry_run();
    }

    for obj in &objects {
        let (api_version, kind) = gvk_of(obj)?;
        let gvk = parse_gvk(&api_version, &kind)?;
        let ar = ApiResource::from_gvk(&gvk);
        let name = obj
            .metadata
            .name
            .clone()
            .context("every object needs metadata.name")?;

        // A Namespace is cluster-scoped; everything else is namespaced into `ns`.
        let api: Api<DynamicObject> = if kind == "Namespace" {
            Api::all_with(client.clone(), &ar)
        } else {
            Api::namespaced_with(client.clone(), ns, &ar)
        };
        api.patch(&name, &params, &Patch::Apply(obj))
            .await
            .with_context(|| format!("applying {kind}/{name} in {ns}"))?;
    }

    Ok(ApplyReport {
        namespace: ns.to_string(),
        applied: names,
        dry_run: state.args.dry_run,
        validated_only: false,
    })
}

fn parse_objects(manifests: &str) -> Result<Vec<DynamicObject>> {
    let mut out = Vec::new();
    for doc in serde_yaml::Deserializer::from_str(manifests) {
        let value = serde_yaml::Value::deserialize(doc).context("parsing a YAML document")?;
        if value.is_null() {
            continue;
        }
        // Round-trip through JSON so the object is exactly what the API server sees.
        let json_value: serde_json::Value =
            serde_json::to_value(&value).context("converting YAML to JSON")?;
        if json_value.get("kind").is_none() {
            continue;
        }
        out.push(serde_json::from_value(json_value).context("not a Kubernetes object")?);
    }
    Ok(out)
}

fn gvk_of(obj: &DynamicObject) -> Result<(String, String)> {
    let t = obj.types.as_ref().context("object has no apiVersion/kind")?;
    Ok((t.api_version.clone(), t.kind.clone()))
}

fn parse_gvk(api_version: &str, kind: &str) -> Result<GroupVersionKind> {
    Ok(match api_version.split_once('/') {
        Some((group, version)) => GroupVersionKind::gvk(group, version, kind),
        None => GroupVersionKind::gvk("", api_version, kind),
    })
}

fn describe(obj: &DynamicObject) -> String {
    let kind = obj.types.as_ref().map(|t| t.kind.clone()).unwrap_or_default();
    let name = obj.metadata.name.clone().unwrap_or_default();
    format!("{kind}/{name}")
}

/// The checks that make this process safe to give a credential to.
fn validate(obj: &DynamicObject, ns: &str) -> Result<()> {
    let (api_version, kind) = gvk_of(obj)?;
    if !ALLOWED_KINDS.iter().any(|(av, k)| *av == api_version && *k == kind) {
        bail!("{api_version}/{kind} is not an allow-listed kind");
    }
    let name = obj.metadata.name.as_deref().context("every object needs metadata.name")?;
    if name.is_empty() || name.len() > 63 {
        bail!("{kind}: metadata.name {name:?} is not a DNS label");
    }

    // The check that matters: an object may not name a namespace other than the one
    // the request is for. A Namespace object may only be the tenant's own.
    match kind.as_str() {
        "Namespace" => {
            if name != ns {
                bail!("a Namespace object must be {ns:?}, got {name:?}");
            }
        }
        _ => match obj.metadata.namespace.as_deref() {
            Some(other) if other != ns => {
                bail!("{kind}/{name} is namespaced into {other:?} but the request is for {ns:?}")
            }
            _ => {}
        },
    }

    // Fields we do not trust yet, wherever they appear in the tree.
    let as_json = serde_json::to_string(obj).unwrap_or_default();
    for key in FORBIDDEN_KEYS {
        if as_json.contains(&format!("\"{key}\"")) {
            bail!("{kind}/{name} uses {key:?}, which is not a verified field on this operator");
        }
    }
    Ok(())
}

/// ADR-0004's drift correction. The platform has no scheduler (a wasm component
/// has no background), so the applier pulls the current revisions and re-applies
/// them. Every apply is idempotent, so this is safe to run at any time — that
/// property is the whole design.
async fn reapply_loop(state: Arc<AppState>, platform_url: String) {
    let period = std::time::Duration::from_secs(state.args.reapply_interval.max(10));
    let http = reqwest::Client::new();
    loop {
        tokio::time::sleep(period).await;
        let url = format!("{}/api/internal/revisions", platform_url.trim_end_matches('/'));
        let res = http
            .get(&url)
            .header("x-platform-secret", &state.args.secret)
            .send()
            .await;
        let revisions = match res {
            Ok(r) if r.status().is_success() => r.json::<serde_json::Value>().await.ok(),
            Ok(r) => {
                eprintln!("applier: re-apply poll got {}", r.status());
                None
            }
            Err(e) => {
                eprintln!("applier: re-apply poll failed: {e}");
                None
            }
        };
        let Some(revisions) = revisions else { continue };
        let list = revisions["revisions"].as_array().cloned().unwrap_or_default();
        let (mut ok, mut failed) = (0usize, 0usize);
        for rev in list {
            let (Some(ns), Some(manifests)) =
                (rev["namespace"].as_str(), rev["manifests"].as_str())
            else {
                continue;
            };
            let req = ApplyRequest { namespace: ns.to_string(), manifests: manifests.to_string() };
            match apply(&state, &req).await {
                Ok(_) => ok += 1,
                Err(e) => {
                    failed += 1;
                    eprintln!("applier: re-apply of {ns} failed: {e:#}");
                }
            }
        }
        if ok + failed > 0 {
            eprintln!("applier: re-applied {ok} deployment(s), {failed} failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(yaml: &str) -> DynamicObject {
        parse_objects(yaml).expect("parses").pop().expect("one object")
    }

    const WORKLOAD: &str = r#"
apiVersion: runtime.wasmcloud.dev/v1alpha1
kind: WorkloadDeployment
metadata:
  name: api
  namespace: tenant-acme
spec:
  replicas: 1
"#;

    #[test]
    fn accepts_a_rendered_workload() {
        validate(&obj(WORKLOAD), "tenant-acme").expect("valid");
    }

    #[test]
    fn refuses_an_object_aimed_at_another_namespace() {
        // The check that turns a wasm-side bug into a 422 instead of a breach.
        let err = validate(&obj(WORKLOAD), "tenant-globex").unwrap_err().to_string();
        assert!(err.contains("namespaced into"), "{err}");
    }

    #[test]
    fn refuses_kinds_outside_the_allow_list() {
        let secret = r#"
apiVersion: v1
kind: Secret
metadata: { name: creds, namespace: tenant-acme }
"#;
        let err = validate(&obj(secret), "tenant-acme").unwrap_err().to_string();
        assert!(err.contains("not an allow-listed kind"), "{err}");
        // ...including the one that would be a privilege escalation.
        let rb = r#"
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata: { name: pwn }
"#;
        assert!(validate(&obj(rb), "tenant-acme").is_err());
    }

    #[test]
    fn refuses_unverified_operator_fields() {
        let with_selector = r#"
apiVersion: runtime.wasmcloud.dev/v1alpha1
kind: WorkloadDeployment
metadata: { name: api, namespace: tenant-acme }
spec:
  template:
    spec:
      hostSelector: { hostgroup: default }
"#;
        let err = validate(&obj(with_selector), "tenant-acme").unwrap_err().to_string();
        assert!(err.contains("hostSelector"), "{err}");
    }

    #[test]
    fn a_namespace_object_must_be_the_tenants_own() {
        let ns = "apiVersion: v1\nkind: Namespace\nmetadata: { name: kube-system }\n";
        assert!(validate(&obj(ns), "tenant-acme").is_err());
        let own = "apiVersion: v1\nkind: Namespace\nmetadata: { name: tenant-acme }\n";
        validate(&obj(own), "tenant-acme").expect("its own namespace is fine");
    }

    #[test]
    fn parses_a_multi_document_render() {
        let both = format!("{WORKLOAD}---\napiVersion: v1\nkind: Service\nmetadata:\n  name: api\n  namespace: tenant-acme\n");
        let objs = parse_objects(&both).unwrap();
        assert_eq!(objs.len(), 2);
        assert_eq!(describe(&objs[0]), "WorkloadDeployment/api");
        assert_eq!(describe(&objs[1]), "Service/api");
        // Comments and the leading generated-by banner must not break parsing.
        let with_comments = format!("# generated\n{both}");
        assert_eq!(parse_objects(&with_comments).unwrap().len(), 2);
    }

    #[test]
    fn gvk_maps_to_the_right_resource() {
        let gvk = parse_gvk("runtime.wasmcloud.dev/v1alpha1", "WorkloadDeployment").unwrap();
        assert_eq!(gvk.group, "runtime.wasmcloud.dev");
        assert_eq!(gvk.version, "v1alpha1");
        let ar = ApiResource::from_gvk(&gvk);
        assert_eq!(ar.plural, "workloaddeployments", "the operator's plural");
        // core/v1 has an empty group
        let core = parse_gvk("v1", "Service").unwrap();
        assert_eq!((core.group.as_str(), core.version.as_str()), ("", "v1"));
    }
}
