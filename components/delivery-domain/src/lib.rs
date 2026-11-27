//! `delivery-domain` — App-agnostic E2E SDLC delivery control plane & SSE dashboard.
//!
//! Tracks deliverables across projects through:
//!   `draft -> gated -> stg -> ab_test -> rollout -> promoted | rolled_back`
//!
//! Zero domain-specific code: reads/writes telemetry through universal
//! `analytics:telemetry@0.1.0` (plug `kv-analytics` locally or
//! `posthog-analytics` for self-hosted PostHog), serves its dashboard via
//! `ui:assets/files@0.1.0`, and exports `delivery:sdlc/router@0.1.0` for
//! context-based strategic component wiring.

pub mod arbitration;
pub mod sdlc;

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/p3",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../../host/wit/deps/comp-store",
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/analytics.wit",
            "../../wit/delivery.wit",
            "../../wit/ui-assets",
            "../record-store/wit",
            "../experiment-assign/wit",
            "../feature-flags/wit",
            "../event-bus/wit",
            "../id-generate/wit",
            "wit",
        ],
        world: "delivery:app/delivery-domain",
        generate_all,
    });
    pub mod p3 {
        pub use super::exports::wasi::http0_3_0_rc_2026_03_15::handler;
        pub use super::wasi::clocks0_3_0_rc_2026_03_15 as clocks;
        pub use super::wasi::http0_3_0_rc_2026_03_15 as http;
    }
}

#[cfg(target_arch = "wasm32")]
mod component {
    use std::collections::BTreeMap;

    use crate::bindings;
    use serde_json::{json, Value};

    use bindings::analytics::telemetry::ingest as analytics_in;
    use bindings::analytics::telemetry::query as analytics_q;
    use bindings::analytics::telemetry::types as atypes;
    use bindings::event::bus::bus;
    use bindings::experiment::assign::assigner as exp;
    use bindings::exports::delivery::sdlc::router::{
        DeliveryError, Guest as RouterGuest, UserContext as WitUserContext,
        WiringDecision as WitWiringDecision,
    };
    use bindings::featureflags::guard::evaluator as flags;
    use bindings::id::generate::generator as ids;
    use bindings::p3::clocks::{monotonic_clock, system_clock};
    use bindings::p3::handler::Guest as HttpGuest;
    use bindings::p3::http::types::{ErrorCode, Fields, Method, Request, Response};
    use bindings::records::store::store as records;
    use bindings::ui::assets::files as assets;

    use super::arbitration::{arbitrate_project, ProjectPolicy, ReleaseStrategy};
    use super::sdlc::{
        aggregate_projects, check_inline_error_guardrail, judge_post_release, Deliverable,
        DeliverableAnalytics, DeliverableDetail, Guardrails, JudgeVerdict, Stage, UserContext,
        VariantMetrics, VariantSpec, WiringPolicy, WiringResolution,
    };

    guestio::guest_p3_respond!();

    struct Component;

    const COLLECTION: &str = "deliverables";
    const POLICIES: &str = "project_policies";
    const EVENTS_ALL: &str = "delivery";
    const POLL_MS: u64 = 500;
    const MAX_TICKS: u32 = 800;

    struct ReqHeaders {
        stage: String,
        tenant: String,
        subject: String,
        cohort: String,
        idempotency_key: String,
    }

    fn extract_headers(req: &Request) -> ReqHeaders {
        let h = req.get_headers();
        let get = |name: &str| -> String {
            h.get(name)
                .into_iter()
                .next()
                .and_then(|v| String::from_utf8(v).ok())
                .unwrap_or_default()
        };
        ReqHeaders {
            stage: get("x-holon-stage"),
            tenant: get("x-holon-tenant"),
            subject: get("x-holon-subject"),
            cohort: get("x-holon-cohort"),
            idempotency_key: get("idempotency-key"),
        }
    }

    impl RouterGuest for Component {
        fn resolve(
            deliverable_id: String,
            ctx: WitUserContext,
        ) -> Result<WitWiringDecision, DeliveryError> {
            let (d, _) = load_deliverable(&deliverable_id).map_err(|_| DeliveryError::NotFound)?;
            let uctx = UserContext {
                tenant: ctx.tenant,
                subject: ctx.subject,
                cohort: ctx.cohort,
                stage: if ctx.stage.is_empty() { "prod".to_string() } else { ctx.stage },
            };
            let res = resolve_for_deliverable(&d, &uctx);
            Ok(WitWiringDecision {
                role: res.role,
                variant_id: res.variant_id,
                digest: res.digest,
                reason: res.reason,
            })
        }
    }

    impl HttpGuest for Component {
        async fn handle(request: Request) -> Result<Response, ErrorCode> {
            let method = request.get_method();
            let path = request.get_path_with_query().unwrap_or_else(|| "/".to_string());
            let route = path.split('?').next().unwrap_or("/").to_string();
            let seg: Vec<&str> = route.trim_matches('/').split('/').collect();
            let hdrs = extract_headers(&request);

            match (&method, seg.as_slice()) {
                (Method::Get, ["api", "stream"]) => stream_events(&path),
                (Method::Get, [""] | ["dashboard"]) => serve_asset(&route),
                _ => {
                    let outcome = match (&method, seg.as_slice()) {
                        (Method::Get, ["health"]) => {
                            Outcome::Json(200, json!({"ok": true}).to_string())
                        }
                        (Method::Get, ["api"]) => usage_json(),
                        (Method::Get, ["api", "projects"]) => list_projects(None),
                        (Method::Get, ["api", "projects", proj]) => list_projects(Some(proj)),
                        (Method::Get, ["api", "projects", proj, "policy"]) => {
                            get_project_policy(proj)
                        }
                        (Method::Post, ["api", "projects", proj, "policy"]) => {
                            set_project_policy(request, proj).await
                        }
                        (Method::Post, ["api", "deliverables"]) => {
                            create_deliverable(request).await
                        }
                        (Method::Get, ["api", "deliverables"]) => list_deliverables(&path),
                        (Method::Get, ["api", "deliverables", id]) => get_deliverable(id),
                        (Method::Get, ["api", "deliverables", id, "timeline"]) => timeline(id),
                        (Method::Get, ["api", "deliverables", id, "analytics"]) => {
                            deliverable_analytics_resp(id)
                        }
                        (Method::Post, ["api", "deliverables", id, "stage"]) => {
                            transition_stage(request, id).await
                        }
                        (Method::Post, ["api", "deliverables", id, "wiring"]) => {
                            update_wiring(request, id).await
                        }
                        (Method::Post, ["api", "deliverables", id, "variants"]) => {
                            register_variant(request, id).await
                        }
                        (Method::Post, ["api", "deliverables", id, "resolve"]) => {
                            resolve_context(request, id, &hdrs).await
                        }
                        (Method::Post, ["api", "deliverables", id, "outcome"]) => {
                            record_outcome(request, id, &hdrs).await
                        }
                        (Method::Post, ["api", "tick"]) => run_tick(),
                        _ => Outcome::Err(404, "not_found".into()),
                    };
                    emit(outcome)
                }
            }
        }
    }

    enum Outcome {
        Json(u16, String),
        Err(u16, String),
    }

    fn now() -> u64 {
        system_clock::now().seconds as u64
    }

    fn serve_asset(route: &str) -> Result<Response, ErrorCode> {
        let target = if route.is_empty() || route == "/" { "/" } else { route };
        match assets::get(target) {
            Some(asset) => {
                let headers = Fields::new();
                let _ = headers.set("content-type", &[asset.content_type.into_bytes()]);
                respond_with(200, headers, asset.body)
            }
            None => emit(Outcome::Err(404, "asset_not_found".into())),
        }
    }

    fn usage_json() -> Outcome {
        Outcome::Json(
            200,
            json!({
                "service": "delivery-domain",
                "about": "App-agnostic E2E SDLC delivery control plane + multi-project SSE dashboard",
                "routes": [
                    "GET  / (Live Multi-Project SSE Dashboard via ui:assets)",
                    "GET  /health",
                    "GET  /api/projects",
                    "GET  /api/projects/{project}",
                    "GET  /api/projects/{project}/policy",
                    "POST /api/projects/{project}/policy",
                    "POST /api/deliverables",
                    "GET  /api/deliverables?project=",
                    "GET  /api/deliverables/{id}",
                    "GET  /api/deliverables/{id}/timeline",
                    "POST /api/deliverables/{id}/stage",
                    "POST /api/deliverables/{id}/wiring",
                    "POST /api/deliverables/{id}/variants",
                    "POST /api/deliverables/{id}/resolve",
                    "POST /api/deliverables/{id}/outcome",
                    "GET  /api/deliverables/{id}/analytics",
                    "POST /api/tick",
                    "GET  /api/stream?after=seq"
                ]
            })
            .to_string(),
        )
    }

    fn exp_name(id: &str) -> String {
        format!("deliv-{id}")
    }

    fn flag_name(id: &str) -> String {
        format!("deliv-rollout-{id}")
    }

    fn topic_for(id: &str) -> String {
        format!("delivery.{id}")
    }

    fn publish_event(deliverable_id: &str, kind: &str, detail: Value) {
        let frame = json!({
            "event_id": ids::short_code(8),
            "deliverable_id": deliverable_id,
            "kind": kind,
            "at": now(),
            "detail": detail,
        });
        let bytes = frame.to_string().into_bytes();
        let _ = bus::publish(&topic_for(deliverable_id), &bytes);
        let _ = bus::publish(EVENTS_ALL, &bytes);
    }

    fn sync_routing_primitives(d: &Deliverable) {
        let (cw, aw) = match d.stage {
            Stage::RolledBack => (100, 0),
            Stage::Promoted => (0, 100),
            _ => (d.control.weight.max(1), d.candidate.weight),
        };
        let arms = vec![
            exp::Arm { name: d.control.id.clone(), weight: cw },
            exp::Arm { name: d.candidate.id.clone(), weight: aw },
        ];
        let _ = exp::set_experiment(&exp_name(&d.id), "", &arms);

        let rule = match d.stage {
            Stage::Promoted => flags::Rule::Enabled,
            Stage::Rollout => flags::Rule::Percentage(d.wiring.rollout_pct.min(100)),
            _ => flags::Rule::Disabled,
        };
        let _ = flags::set_rule(&flag_name(&d.id), "", rule);
    }

    fn load_deliverable(id: &str) -> Result<(Deliverable, u64), Outcome> {
        let entry = records::get(COLLECTION, id).map_err(|e| match e {
            records::StoreError::NotFound => Outcome::Err(404, "deliverable not found".into()),
            other => Outcome::Err(503, format!("store error: {other:?}")),
        })?;
        let d: Deliverable = serde_json::from_str(&entry.data)
            .map_err(|e| Outcome::Err(500, format!("corrupt deliverable record: {e}")))?;
        Ok((d, entry.revision))
    }

    fn save_deliverable(d: &Deliverable, rev: u64) -> Result<(), Outcome> {
        let data = serde_json::to_string(d)
            .map_err(|e| Outcome::Err(500, format!("serialize deliverable: {e}")))?;
        records::update(COLLECTION, &d.id, &data, rev)
            .map_err(|e| Outcome::Err(409, format!("update failed: {e:?}")))?;
        sync_routing_primitives(d);
        Ok(())
    }

    fn compute_analytics(d: &Deliverable) -> DeliverableAnalytics {
        let req = atypes::ExperimentQuery {
            project: d.project.clone(),
            experiment_key: d.id.clone(),
            exposure_event: "exposed".to_string(),
            conversion_event: "converted".to_string(),
            error_event: "error".to_string(),
            variant_property: "variant_id".to_string(),
            variants: vec![d.control.id.clone(), d.candidate.id.clone()],
            window: atypes::TimeWindow { from_unix: 0, to_unix: 0 },
        };
        let summaries = analytics_q::experiment_results(&req).unwrap_or_default();
        let to_metrics = |v: &VariantSpec| -> VariantMetrics {
            if let Some(s) = summaries.iter().find(|r| r.arm == v.id) {
                VariantMetrics {
                    variant_id: v.id.clone(),
                    role: v.role.clone(),
                    exposures: s.exposures,
                    conversions: s.conversions,
                    errors: s.errors,
                    conversion_rate: s.conversion_rate,
                    error_rate: s.error_rate,
                }
            } else {
                VariantMetrics {
                    variant_id: v.id.clone(),
                    role: v.role.clone(),
                    exposures: 0,
                    conversions: 0,
                    errors: 0,
                    conversion_rate: 0.0,
                    error_rate: 0.0,
                }
            }
        };
        let control = to_metrics(&d.control);
        let candidate = to_metrics(&d.candidate);
        let conversion_lift = candidate.conversion_rate - control.conversion_rate;
        DeliverableAnalytics {
            deliverable_id: d.id.clone(),
            stage: d.stage,
            control,
            candidate,
            conversion_lift,
        }
    }

    fn merge_context(body: &Value, hdrs: &ReqHeaders) -> UserContext {
        let pick = |field: &str, hdr: &str| -> String {
            body["context"][field]
                .as_str()
                .or_else(|| body[field].as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| hdr.to_string())
        };
        let stage = {
            let s = pick("stage", &hdrs.stage);
            if s.is_empty() {
                "prod".to_string()
            } else {
                s
            }
        };
        UserContext {
            tenant: pick("tenant", &hdrs.tenant),
            subject: pick("subject", &hdrs.subject),
            cohort: pick("cohort", &hdrs.cohort),
            stage,
        }
    }

    fn resolve_for_deliverable(d: &Deliverable, ctx: &UserContext) -> WiringResolution {
        let subject = if ctx.subject.is_empty() { "anonymous" } else { ctx.subject.as_str() };
        let ab_arm = exp::assign(
            &exp_name(&d.id),
            &exp::Context { tenant: ctx.tenant.clone(), subject: subject.to_string() },
        )
        .ok();
        let flag_enabled = flags::is_enabled(
            &flag_name(&d.id),
            &flags::Context { tenant: ctx.tenant.clone(), subject: subject.to_string() },
        )
        .unwrap_or(false);
        d.resolve_wiring(ctx, ab_arm.as_deref(), flag_enabled)
    }

    fn load_project_deliverables(project: &str) -> Vec<Deliverable> {
        records::find_by(COLLECTION, "project", &json!(project).to_string())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| serde_json::from_str::<Deliverable>(&e.data).ok())
            .collect()
    }

    fn load_policy(project: &str) -> (ProjectPolicy, Option<(String, u64)>) {
        let entries =
            records::find_by(POLICIES, "project", &json!(project).to_string()).unwrap_or_default();
        if let Some(e) = entries.into_iter().next() {
            if let Ok(p) = serde_json::from_str::<ProjectPolicy>(&e.data) {
                return (p, Some((e.id, e.revision)));
            }
        }
        (ProjectPolicy::default_for(project), None)
    }

    fn get_project_policy(project: &str) -> Outcome {
        let (policy, _) = load_policy(project);
        let deliverables = load_project_deliverables(project);
        let arbitration = arbitrate_project(&policy, &deliverables);
        Outcome::Json(200, json!({ "policy": policy, "arbitration": arbitration }).to_string())
    }

    async fn set_project_policy(request: Request, project: &str) -> Outcome {
        let body = match parse_body(request).await {
            Ok(v) => v,
            Err(o) => return o,
        };
        let (mut policy, existing) = load_policy(project);
        if let Some(s) = body["strategy"].as_str() {
            let Some(parsed) = ReleaseStrategy::parse(s) else {
                return Outcome::Err(422, format!("unknown strategy: {s:?}"));
            };
            policy.strategy = parsed;
        }
        if let Some(n) = body["max_concurrent_rollouts"].as_u64() {
            policy.max_concurrent_rollouts = (n as usize).max(1);
        }
        if let Some(arr) = body["picked_ids"].as_array() {
            policy.picked_ids = arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
        if let Some(pick_id) = body["pick"].as_str().filter(|s| !s.is_empty()) {
            if !policy.picked_ids.iter().any(|x| x == pick_id) {
                policy.picked_ids.push(pick_id.to_string());
            }
        }
        if let Some(obj) = body["priorities"].as_object() {
            for (k, v) in obj {
                if let Some(p) = v.as_u64() {
                    policy.priorities.insert(k.clone(), p as u32);
                }
            }
        }
        let data = match serde_json::to_string(&policy) {
            Ok(s) => s,
            Err(e) => return Outcome::Err(500, format!("serialize policy: {e}")),
        };
        let save_res = match existing {
            Some((rec_id, rev)) => records::update(POLICIES, &rec_id, &data, rev).map(|_| ()),
            None => records::create(POLICIES, &data, &["project".to_string()]).map(|_| ()),
        };
        if let Err(e) = save_res {
            return Outcome::Err(503, format!("save policy failed: {e:?}"));
        }
        let deliverables = load_project_deliverables(project);
        let arbitration = arbitrate_project(&policy, &deliverables);
        publish_event(
            project,
            "policy_updated",
            json!({
                "project": project,
                "strategy": policy.strategy.as_str(),
                "max_concurrent_rollouts": policy.max_concurrent_rollouts,
                "picked_ids": policy.picked_ids,
            }),
        );
        Outcome::Json(200, json!({ "policy": policy, "arbitration": arbitration }).to_string())
    }

    fn list_projects(only: Option<&str>) -> Outcome {
        let entries =
            records::list_records(COLLECTION, 200, "").map(|p| p.entries).unwrap_or_default();
        let details: Vec<DeliverableDetail> = entries
            .into_iter()
            .filter_map(|e| {
                let d: Deliverable = serde_json::from_str(&e.data).ok()?;
                if let Some(filter) = only {
                    if d.project != filter {
                        return None;
                    }
                }
                let analytics = compute_analytics(&d);
                let judge = judge_post_release(&d, &analytics);
                Some(DeliverableDetail { deliverable: d, analytics, judge })
            })
            .collect();

        let projects = aggregate_projects(details);
        let total_deliverables: usize = projects.iter().map(|p| p.total_deliverables).sum();
        let active_deliverables: usize = projects
            .iter()
            .flat_map(|p| p.deliverables.iter())
            .filter(|d| matches!(d.deliverable.stage, Stage::Stg | Stage::AbTest | Stage::Rollout))
            .count();
        let rolled_back: usize = projects
            .iter()
            .flat_map(|p| p.deliverables.iter())
            .filter(|d| d.deliverable.stage == Stage::RolledBack)
            .count();
        let total_exposures: u64 = projects.iter().map(|p| p.total_exposures).sum();
        let total_conversions: u64 = projects.iter().map(|p| p.total_conversions).sum();
        let total_errors: u64 = projects.iter().map(|p| p.total_errors).sum();
        let avg_conversion_lift = if projects.is_empty() {
            0.0
        } else {
            projects.iter().map(|p| p.avg_conversion_lift).sum::<f64>() / projects.len() as f64
        };

        let enriched_projects: Vec<Value> = projects
            .iter()
            .map(|p| {
                let (policy, _) = load_policy(&p.project);
                let ds: Vec<Deliverable> =
                    p.deliverables.iter().map(|x| x.deliverable.clone()).collect();
                let arbitration = arbitrate_project(&policy, &ds);
                let mut v = serde_json::to_value(p).unwrap_or_default();
                v["policy"] = serde_json::to_value(&policy).unwrap_or_default();
                v["arbitration"] = serde_json::to_value(&arbitration).unwrap_or_default();
                v
            })
            .collect();

        Outcome::Json(
            200,
            json!({
                "portfolio": {
                    "total_projects": projects.len(),
                    "total_deliverables": total_deliverables,
                    "active_deliverables": active_deliverables,
                    "rolled_back": rolled_back,
                    "total_exposures": total_exposures,
                    "total_conversions": total_conversions,
                    "total_errors": total_errors,
                    "avg_conversion_lift": avg_conversion_lift,
                },
                "projects": enriched_projects,
            })
            .to_string(),
        )
    }

    async fn create_deliverable(request: Request) -> Outcome {
        let body = match parse_body(request).await {
            Ok(v) => v,
            Err(o) => return o,
        };
        let name = body["name"].as_str().unwrap_or("").trim().to_string();
        if name.is_empty() {
            return Outcome::Err(422, "name is required".into());
        }
        let project = body["project"].as_str().unwrap_or("default").to_string();
        let capability = body["capability"].as_str().unwrap_or("app:feature@0.1.0").to_string();
        let goal_ref = body["goal_ref"].as_str().unwrap_or("").to_string();
        let stage = body["stage"].as_str().and_then(Stage::parse).unwrap_or(Stage::Draft);

        let control: VariantSpec =
            serde_json::from_value(body["control"].clone()).unwrap_or_else(|_| VariantSpec {
                id: "control".to_string(),
                role: "control".to_string(),
                language: "wasm".to_string(),
                digest: String::new(),
                weight: 50,
                endpoint: None,
                config: BTreeMap::new(),
            });
        let candidate: VariantSpec = serde_json::from_value(body["candidate"].clone())
            .unwrap_or_else(|_| VariantSpec {
                id: "candidate".to_string(),
                role: "candidate".to_string(),
                language: "wasm".to_string(),
                digest: String::new(),
                weight: 50,
                endpoint: None,
                config: BTreeMap::new(),
            });
        let mut wiring: WiringPolicy =
            serde_json::from_value(body["wiring"].clone()).unwrap_or_default();
        if stage == Stage::Rollout && wiring.rollout_pct > 0 {
            wiring.align_step();
        }
        let guardrails: Guardrails =
            serde_json::from_value(body["guardrails"].clone()).unwrap_or_default();

        let ts = now();
        let draft = Deliverable {
            id: String::new(),
            project,
            name,
            capability,
            goal_ref,
            stage,
            control,
            candidate,
            wiring,
            guardrails,
            created_at: ts,
            updated_at: ts,
        };
        let initial_json = match serde_json::to_string(&draft) {
            Ok(s) => s,
            Err(e) => return Outcome::Err(500, format!("serialize: {e}")),
        };
        let entry = match records::create(COLLECTION, &initial_json, &["project".to_string()]) {
            Ok(e) => e,
            Err(e) => return Outcome::Err(503, format!("create failed: {e:?}")),
        };
        let mut stored = draft;
        stored.id = entry.id.clone();
        if let Err(o) = save_deliverable(&stored, entry.revision) {
            return o;
        }
        publish_event(
            &stored.id,
            "deliverable_created",
            json!({
                "project": stored.project,
                "name": stored.name,
                "stage": stored.stage.as_str(),
                "control": stored.control.id,
                "candidate": stored.candidate.id,
            }),
        );
        Outcome::Json(201, serde_json::to_string(&stored).unwrap_or_default())
    }

    fn list_deliverables(path: &str) -> Outcome {
        let project_filter = query_str(path, "project").unwrap_or_default();
        let entries = if project_filter.is_empty() {
            records::list_records(COLLECTION, 100, "").map(|p| p.entries).unwrap_or_default()
        } else {
            records::find_by(COLLECTION, "project", &json!(project_filter).to_string())
                .unwrap_or_default()
        };
        let items: Vec<Value> = entries
            .into_iter()
            .filter_map(|e| {
                let d: Deliverable = serde_json::from_str(&e.data).ok()?;
                let a = compute_analytics(&d);
                Some(json!({ "deliverable": d, "analytics": a }))
            })
            .collect();
        Outcome::Json(200, json!({ "deliverables": items }).to_string())
    }

    fn get_deliverable(id: &str) -> Outcome {
        let (d, _) = match load_deliverable(id) {
            Ok(v) => v,
            Err(o) => return o,
        };
        let analytics = compute_analytics(&d);
        let judge = judge_post_release(&d, &analytics);
        Outcome::Json(
            200,
            json!({ "deliverable": d, "analytics": analytics, "judge": judge }).to_string(),
        )
    }

    fn deliverable_analytics_resp(id: &str) -> Outcome {
        let (d, _) = match load_deliverable(id) {
            Ok(v) => v,
            Err(o) => return o,
        };
        let analytics = compute_analytics(&d);
        Outcome::Json(200, serde_json::to_string(&analytics).unwrap_or_default())
    }

    fn timeline(id: &str) -> Outcome {
        let events = bus::poll(&topic_for(id), "timeline", 4096).unwrap_or_default();
        let mut rows: Vec<(i64, Value)> = events
            .into_iter()
            .filter_map(|e| {
                let seq: i64 = e.id.parse().ok()?;
                let mut v: Value = serde_json::from_slice(&e.payload).ok()?;
                v["seq"] = json!(seq);
                Some((seq, v))
            })
            .collect();
        rows.sort_by_key(|(seq, _)| *seq);
        let out: Vec<Value> = rows.into_iter().map(|(_, v)| v).collect();
        Outcome::Json(200, json!({ "deliverable_id": id, "events": out }).to_string())
    }

    async fn transition_stage(request: Request, id: &str) -> Outcome {
        let body = match parse_body(request).await {
            Ok(v) => v,
            Err(o) => return o,
        };
        let next_str = body["stage"].as_str().unwrap_or("");
        let Some(next_stage) = Stage::parse(next_str) else {
            return Outcome::Err(422, format!("unknown stage: {next_str:?}"));
        };
        let (mut d, rev) = match load_deliverable(id) {
            Ok(v) => v,
            Err(o) => return o,
        };
        let force = body["force"].as_bool().unwrap_or(false);
        if !force && !d.stage.can_transition_to(next_stage) {
            return Outcome::Err(
                409,
                format!("illegal transition {} -> {}", d.stage.as_str(), next_stage.as_str()),
            );
        }
        if !force && matches!(next_stage, Stage::AbTest | Stage::Rollout) {
            let (policy, _) = load_policy(&d.project);
            let siblings = load_project_deliverables(&d.project);
            let arb = arbitrate_project(&policy, &siblings);
            if let Some(reason) = arb.hold_reason(&d.id) {
                return Outcome::Err(409, format!("blocked by project policy: {reason}"));
            }
        }
        let prev = d.stage;
        d.stage = next_stage;
        if let Some(pct) = body["rollout_pct"].as_u64() {
            d.wiring.rollout_pct = pct.min(100) as u8;
            d.wiring.align_step();
        } else if next_stage == Stage::Rollout && d.wiring.rollout_pct == 0 {
            d.wiring.rollout_pct = d.wiring.rollout_steps.first().copied().unwrap_or(10);
            d.wiring.align_step();
        } else if next_stage == Stage::Promoted {
            d.wiring.rollout_pct = 100;
            d.wiring.align_step();
        } else if next_stage == Stage::RolledBack {
            d.wiring.rollout_pct = 0;
            d.wiring.current_step = 0;
            d.control.weight = 100;
            d.candidate.weight = 0;
        }
        d.updated_at = now();
        if let Err(o) = save_deliverable(&d, rev) {
            return o;
        }
        let reason = body["reason"].as_str().unwrap_or("manual").to_string();
        publish_event(
            &d.id,
            "stage_changed",
            json!({
                "project": d.project,
                "from": prev.as_str(),
                "to": d.stage.as_str(),
                "rollout_pct": d.wiring.rollout_pct,
                "reason": reason,
            }),
        );
        Outcome::Json(200, serde_json::to_string(&d).unwrap_or_default())
    }

    async fn update_wiring(request: Request, id: &str) -> Outcome {
        let body = match parse_body(request).await {
            Ok(v) => v,
            Err(o) => return o,
        };
        let (mut d, rev) = match load_deliverable(id) {
            Ok(v) => v,
            Err(o) => return o,
        };
        if let Some(arr) = body["stg_tenants"].as_array() {
            d.wiring.stg_tenants =
                arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
        if let Some(arr) = body["stg_cohorts"].as_array() {
            d.wiring.stg_cohorts =
                arr.iter().filter_map(|v| v.as_str().map(str::to_string)).collect();
        }
        if let Some(allow) = body["allow_stage_header"].as_bool() {
            d.wiring.allow_stage_header = allow;
        }
        if let Some(steps) = body["rollout_steps"].as_array() {
            let parsed: Vec<u8> =
                steps.iter().filter_map(|v| v.as_u64().map(|n| n.min(100) as u8)).collect();
            if !parsed.is_empty() {
                d.wiring.rollout_steps = parsed;
            }
        }
        if let Some(pct) = body["rollout_pct"].as_u64() {
            d.wiring.rollout_pct = pct.min(100) as u8;
            d.wiring.align_step();
        }
        if let Some(w) = body["control_weight"].as_u64() {
            d.control.weight = w as u32;
        }
        if let Some(w) = body["candidate_weight"].as_u64() {
            d.candidate.weight = w as u32;
        }
        d.updated_at = now();
        if let Err(o) = save_deliverable(&d, rev) {
            return o;
        }
        publish_event(
            &d.id,
            "wiring_updated",
            json!({
                "project": d.project,
                "wiring": d.wiring,
                "control_weight": d.control.weight,
                "candidate_weight": d.candidate.weight,
            }),
        );
        Outcome::Json(200, serde_json::to_string(&d).unwrap_or_default())
    }

    async fn register_variant(request: Request, id: &str) -> Outcome {
        let body = match parse_body(request).await {
            Ok(v) => v,
            Err(o) => return o,
        };
        let (mut d, rev) = match load_deliverable(id) {
            Ok(v) => v,
            Err(o) => return o,
        };
        let role = body["role"].as_str().unwrap_or("candidate");
        let target = if role == "control" { &mut d.control } else { &mut d.candidate };
        if let Some(vid) = body["id"].as_str().filter(|s| !s.is_empty()) {
            target.id = vid.to_string();
        }
        if let Some(lang) = body["language"].as_str().filter(|s| !s.is_empty()) {
            target.language = lang.to_string();
        }
        if let Some(dig) = body["digest"].as_str() {
            target.digest = dig.to_string();
        }
        if let Some(w) = body["weight"].as_u64() {
            target.weight = w as u32;
        }
        if body.get("endpoint").is_some() {
            target.endpoint = body["endpoint"].as_str().map(str::to_string);
        }
        if let Some(obj) = body["config"].as_object() {
            for (k, v) in obj {
                if let Some(val) = v.as_str() {
                    target.config.insert(k.clone(), val.to_string());
                }
            }
        }
        let registered = target.clone();
        d.updated_at = now();
        if let Err(o) = save_deliverable(&d, rev) {
            return o;
        }
        publish_event(
            &d.id,
            "variant_registered",
            json!({
                "project": d.project,
                "role": registered.role,
                "variant_id": registered.id,
                "language": registered.language,
                "digest": registered.digest,
            }),
        );
        Outcome::Json(200, serde_json::to_string(&d).unwrap_or_default())
    }

    async fn resolve_context(request: Request, id: &str, hdrs: &ReqHeaders) -> Outcome {
        let body = match parse_body(request).await {
            Ok(v) => v,
            Err(o) => return o,
        };
        let (d, _) = match load_deliverable(id) {
            Ok(v) => v,
            Err(o) => return o,
        };
        let ctx = merge_context(&body, hdrs);
        let res = resolve_for_deliverable(&d, &ctx);
        Outcome::Json(
            200,
            json!({
                "deliverable_id": d.id,
                "stage": d.stage.as_str(),
                "context": ctx,
                "wiring": res,
            })
            .to_string(),
        )
    }

    async fn record_outcome(request: Request, id: &str, hdrs: &ReqHeaders) -> Outcome {
        let body = match parse_body(request).await {
            Ok(v) => v,
            Err(o) => return o,
        };
        let (mut d, rev) = match load_deliverable(id) {
            Ok(v) => v,
            Err(o) => return o,
        };
        let kind = match body["kind"].as_str().unwrap_or("converted") {
            "converted" | "conversion" => "converted",
            "error" | "failure" => "error",
            "exposed" => "exposed",
            other => return Outcome::Err(422, format!("unknown outcome kind: {other:?}")),
        };
        let ctx = merge_context(&body, hdrs);
        let variant_id = if let Some(v) = body["variant_id"].as_str().filter(|s| !s.is_empty()) {
            v.to_string()
        } else {
            resolve_for_deliverable(&d, &ctx).variant_id
        };

        let idem_key = body["idempotency_key"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| hdrs.idempotency_key.clone());

        let mut props = vec![
            atypes::Property { key: "experiment_key".into(), value: d.id.clone() },
            atypes::Property { key: "variant_id".into(), value: variant_id.clone() },
        ];
        if !idem_key.is_empty() {
            props.push(atypes::Property { key: "idempotency_key".into(), value: idem_key });
        }

        let _ = analytics_in::capture(&atypes::Event {
            name: kind.to_string(),
            distinct_id: if ctx.subject.is_empty() { "anonymous".into() } else { ctx.subject },
            project: d.project.clone(),
            timestamp_unix: now(),
            properties: props,
        });

        publish_event(
            &d.id,
            "outcome_recorded",
            json!({
                "project": d.project,
                "variant_id": variant_id,
                "kind": kind,
            }),
        );

        let mut auto_rolled_back = false;
        let mut rollback_reason: Option<String> = None;
        if kind == "error" {
            let analytics = compute_analytics(&d);
            if let Some(reason) = check_inline_error_guardrail(&d, &analytics) {
                let prev = d.stage;
                d.stage = Stage::RolledBack;
                d.wiring.rollout_pct = 0;
                d.wiring.current_step = 0;
                d.control.weight = 100;
                d.candidate.weight = 0;
                d.updated_at = now();
                if save_deliverable(&d, rev).is_ok() {
                    auto_rolled_back = true;
                    rollback_reason = Some(reason.clone());
                    publish_event(
                        &d.id,
                        "auto_rollback",
                        json!({
                            "project": d.project,
                            "from": prev.as_str(),
                            "to": "rolled_back",
                            "inline": true,
                            "reason": reason,
                            "analytics": analytics,
                        }),
                    );
                }
            }
        }

        Outcome::Json(
            200,
            json!({
                "deliverable_id": d.id,
                "variant_id": variant_id,
                "kind": kind,
                "auto_rolled_back": auto_rolled_back,
                "rollback_reason": rollback_reason,
            })
            .to_string(),
        )
    }

    fn run_tick() -> Outcome {
        let entries =
            records::list_records(COLLECTION, 100, "").map(|p| p.entries).unwrap_or_default();
        let mut actions = Vec::new();
        for e in entries {
            let Ok(mut d) = serde_json::from_str::<Deliverable>(&e.data) else {
                continue;
            };
            let analytics = compute_analytics(&d);
            let verdict = judge_post_release(&d, &analytics);
            match &verdict {
                JudgeVerdict::Hold { .. } => {}
                JudgeVerdict::Rollback { reason } => {
                    let prev = d.stage;
                    d.stage = Stage::RolledBack;
                    d.wiring.rollout_pct = 0;
                    d.wiring.current_step = 0;
                    d.control.weight = 100;
                    d.candidate.weight = 0;
                    d.updated_at = now();
                    if save_deliverable(&d, e.revision).is_ok() {
                        publish_event(
                            &d.id,
                            "auto_rollback",
                            json!({
                                "project": d.project,
                                "from": prev.as_str(),
                                "to": "rolled_back",
                                "inline": false,
                                "reason": reason,
                                "analytics": analytics,
                            }),
                        );
                    }
                }
                JudgeVerdict::Advance { next_stage, next_rollout_pct, next_step, reason } => {
                    let prev = d.stage;
                    d.stage = *next_stage;
                    d.wiring.rollout_pct = *next_rollout_pct;
                    d.wiring.current_step = *next_step;
                    d.updated_at = now();
                    if save_deliverable(&d, e.revision).is_ok() {
                        publish_event(
                            &d.id,
                            "auto_advance",
                            json!({
                                "project": d.project,
                                "from": prev.as_str(),
                                "to": d.stage.as_str(),
                                "rollout_pct": d.wiring.rollout_pct,
                                "reason": reason,
                                "analytics": analytics,
                            }),
                        );
                    }
                }
            }
            actions.push(json!({
                "deliverable_id": d.id,
                "verdict": verdict,
            }));
        }
        Outcome::Json(200, json!({ "evaluated": actions.len(), "actions": actions }).to_string())
    }

    fn stream_events(path: &str) -> Result<Response, ErrorCode> {
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"text/event-stream".to_vec()]);
        let _ = headers.set("cache-control", &[b"no-cache".to_vec()]);
        let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);

        let mut cursor = query_i64(path, "after").unwrap_or_else(current_seq);
        let (mut stream, rx) = bindings::wit_stream::new();
        wit_bindgen::spawn_local(async move {
            if !stream.write_all(b": connected\n\n".to_vec()).await.is_empty() {
                return;
            }
            for _ in 0..MAX_TICKS {
                let (rows, new_cursor) = events_after(cursor);
                cursor = new_cursor;
                let frame = if rows.is_empty() {
                    ": ping\n\n".to_string()
                } else {
                    rows.iter().map(|r| format!("data: {r}\n\n")).collect::<String>()
                };
                if !stream.write_all(frame.into_bytes()).await.is_empty() {
                    break;
                }
                monotonic_clock::wait_for(POLL_MS * 1_000_000).await;
            }
        });
        let (trailers_tx, trailers_rx) = bindings::wit_future::new(|| Ok(None));
        drop(trailers_tx);
        let (response, _sent) = Response::new(headers, Some(rx), trailers_rx);
        let _ = response.set_status_code(200);
        Ok(response)
    }

    fn events_after(after: i64) -> (Vec<Value>, i64) {
        let events = bus::poll(EVENTS_ALL, "stream", 4096).unwrap_or_default();
        let mut rows: Vec<(i64, Value)> = events
            .iter()
            .filter_map(|e| {
                let seq: i64 = e.id.parse().ok()?;
                let mut v: Value = serde_json::from_slice(&e.payload).ok()?;
                (seq > after).then(|| {
                    v["seq"] = json!(seq);
                    (seq, v)
                })
            })
            .collect();
        rows.sort_by_key(|(seq, _)| *seq);
        let cursor = rows.last().map(|(seq, _)| *seq).unwrap_or(after);
        (rows.into_iter().map(|(_, v)| v).collect(), cursor)
    }

    fn current_seq() -> i64 {
        bus::poll(EVENTS_ALL, "stream", 4096)
            .unwrap_or_default()
            .iter()
            .filter_map(|e| e.id.parse::<i64>().ok())
            .max()
            .unwrap_or(-1)
    }

    async fn parse_body(request: Request) -> Result<Value, Outcome> {
        let body = read_body(request)
            .await
            .map_err(|_| Outcome::Err(400, "could not read body".into()))?;
        if body.is_empty() {
            return Ok(Value::Object(Default::default()));
        }
        serde_json::from_slice(&body).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
    }

    const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
    guestio::guest_p3_read_body!(MAX_BODY_BYTES);

    fn query_str(path: &str, key: &str) -> Option<String> {
        let query = path.split('?').nth(1)?;
        query.split('&').find_map(|pair| {
            let mut it = pair.splitn(2, '=');
            (it.next()? == key).then(|| decode(it.next().unwrap_or("")))
        })
    }

    fn query_i64(path: &str, key: &str) -> Option<i64> {
        query_str(path, key)?.parse().ok()
    }

    use guestfmt::percent_decode as decode;

    fn emit(result: Outcome) -> Result<Response, ErrorCode> {
        match result {
            Outcome::Json(code, body) => json_response(code, body),
            Outcome::Err(code, msg) => json_response(code, json!({ "error": msg }).to_string()),
        }
    }

    fn json_response(status: u16, body: String) -> Result<Response, ErrorCode> {
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let _ = headers.set("access-control-allow-origin", &[b"*".to_vec()]);
        respond_with(status, headers, body.into_bytes())
    }

    bindings::export!(Component with_types_in bindings);
}
