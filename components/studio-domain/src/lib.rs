//! `studio-domain` — the composition studio (docs/apps/STUDIO.md) as ONE composed wasm HTTP
//! component. Exports `wasi:http`; imports only contracts: `wit:reflect` (every
//! interesting answer), `records:store` (surfaces + saved canvases), `blob:store`
//! (the uploaded bytes, needed again at compose time).
//!
//! The app is deliberately thin. It does three things `wit:reflect` shouldn't:
//! remembers what you uploaded, resolves canvas node ids to stored surfaces and
//! bytes, and turns results into JSON. Every decision — does this plug fit, what
//! order do these build in, what does the manifest look like, what are the
//! composed bytes — belongs to the component.
//!
//! One deliberate shape: `/api/plan`, `/api/emit` and `/api/compose` take node
//! IDS, not surfaces. The browser sends `["mesh-domain", "record-store"]` and the
//! server rehydrates from storage, so a canvas of 20 components is a 200-byte
//! request instead of a megabyte of duplicated WIT.

#[allow(warnings)]
mod bindings;

use serde_json::{json, Map, Value};

use bindings::blob::store::blobstore as blob;
use bindings::records::store::store as records;
use bindings::wasi::clocks::wall_clock;
use bindings::wit::reflect::composer as composer;
use bindings::wit::reflect::inspector as inspector;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::wasi::http::types::{
    Fields, IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};

struct Component;

const COMPONENTS: &str = "components";
const GRAPHS: &str = "graphs";
/// blob:store container for the raw uploads.
const BIN: &str = "wasm";

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let method = request.method();
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let (route, query) = split_query(&path);
        let seg: Vec<&str> = route.trim_matches('/').split('/').collect();

        let outcome = match (&method, seg.as_slice()) {
            (Method::Get, [""]) => usage(),
            (Method::Post, ["api", "components"]) => component_add(&request, &query),
            (Method::Get, ["api", "components"]) => components_list(),
            (Method::Post, ["api", "components", "delete"]) => component_delete(&request),
            (Method::Post, ["api", "plan"]) => plan_route(&request),
            (Method::Post, ["api", "satisfies"]) => satisfies_route(&request),
            (Method::Post, ["api", "emit"]) => emit_route(&request),
            (Method::Post, ["api", "compose"]) => compose_route(&request),
            (Method::Get, ["api", "graphs"]) => graphs_list(),
            (Method::Post, ["api", "graphs"]) => graph_save(&request, None),
            (Method::Get, ["api", "graphs", id]) => graph_get(id),
            (Method::Put, ["api", "graphs", id]) | (Method::Post, ["api", "graphs", id]) => {
                graph_save(&request, Some(id))
            }
            _ => Outcome::Err(404, "not_found".into()),
        };
        emit_response(response_out, outcome);
    }
}

enum Outcome {
    Json(u16, String),
    /// status, headers, bytes — the composed component and the text forms.
    Raw(u16, Vec<(String, String)>, Vec<u8>),
    Err(u16, String),
}

/// A raw body with just a content type.
fn raw(code: u16, content_type: &str, bytes: Vec<u8>) -> Outcome {
    Outcome::Raw(code, vec![("content-type".to_string(), content_type.to_string())], bytes)
}

fn now() -> u64 {
    wall_clock::now().seconds
}

fn split_query(path: &str) -> (String, Map<String, Value>) {
    let mut parts = path.splitn(2, '?');
    let route = parts.next().unwrap_or("/").to_string();
    let mut q = Map::new();
    if let Some(raw) = parts.next() {
        for pair in raw.split('&') {
            if let Some((k, v)) = pair.split_once('=') {
                q.insert(k.to_string(), json!(percent_decode(v)));
            }
        }
    }
    (route, q)
}

/// Enough of percent-decoding for a component id in a query string.
fn percent_decode(s: &str) -> String {
    let bytes = s.replace('+', " ").into_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(b) = u8::from_str_radix(&String::from_utf8_lossy(&bytes[i + 1..i + 3]), 16) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn usage() -> Outcome {
    Outcome::Json(
        200,
        json!({
            "service": "studio",
            "about": "a composition studio for wasm components — reflect a .wasm, wire a type-checked graph, and emit or build the wac / wasmCloud form of it",
            "components": "POST /api/components?id=NAME (raw .wasm body), GET /api/components, POST /api/components/delete {id}",
            "graph": "POST /api/plan {nodes:[id], edges:[{plug,socket,iface}]}, POST /api/satisfies {socket, plug}",
            "emit": "POST /api/emit {nodes, edges, form: plug|wac|workload, meta?}",
            "compose": "POST /api/compose {nodes, edges, root} -> a real composed component",
            "graphs": "GET/POST /api/graphs, GET/PUT /api/graphs/{id}"
        })
        .to_string(),
    )
}

// ---- the palette: reflected components --------------------------------------

/// A stored surface, as JSON. `wit:reflect` owns the shape; we only transcribe.
fn surface_json(s: &inspector::Surface) -> Value {
    let refs = |list: &Vec<inspector::IfaceRef>| -> Vec<Value> {
        list.iter()
            .map(|r| json!({ "raw": r.raw, "namespace": r.namespace, "pkg": r.pkg, "name": r.name, "version": r.version }))
            .collect()
    };
    json!({
        "name": s.name,
        "exports": refs(&s.exports),
        "imports": refs(&s.imports),
        "host_imports": refs(&s.host_imports),
        "size_bytes": s.size_bytes,
        "sha256": s.sha256,
        "nested_instances": s.nested_instances,
    })
}

fn iface_from(v: &Value) -> inspector::IfaceRef {
    inspector::IfaceRef {
        raw: v["raw"].as_str().unwrap_or_default().to_string(),
        namespace: v["namespace"].as_str().unwrap_or_default().to_string(),
        pkg: v["pkg"].as_str().unwrap_or_default().to_string(),
        name: v["name"].as_str().unwrap_or_default().to_string(),
        version: v["version"].as_str().unwrap_or_default().to_string(),
    }
}

fn surface_from(v: &Value) -> inspector::Surface {
    let list = |key: &str| -> Vec<inspector::IfaceRef> {
        v[key].as_array().map(|a| a.iter().map(iface_from).collect()).unwrap_or_default()
    };
    inspector::Surface {
        name: v["name"].as_str().unwrap_or_default().to_string(),
        exports: list("exports"),
        imports: list("imports"),
        host_imports: list("host_imports"),
        size_bytes: v["size_bytes"].as_u64().unwrap_or(0),
        sha256: v["sha256"].as_str().unwrap_or_default().to_string(),
        nested_instances: v["nested_instances"].as_u64().unwrap_or(0) as u32,
    }
}

/// Upload a component: reflect it, keep the surface for the palette and the bytes
/// for composing. The id defaults to the component's own name section, so
/// `POST /api/components` with no id still lands as `mesh-domain`.
fn component_add(request: &IncomingRequest, query: &Map<String, Value>) -> Outcome {
    let bytes = match read_body(request) {
        Ok(b) if !b.is_empty() => b,
        Ok(_) => return Outcome::Err(422, "empty body — POST the raw .wasm".into()),
        Err(_) => return Outcome::Err(400, "could not read body".into()),
    };

    // Reflection doubles as validation: a truncated upload fails here rather than
    // becoming a broken palette entry.
    let surface = match inspector::inspect(&bytes) {
        Ok(s) => s,
        Err(e) => return Outcome::Err(422, reflect_error(&e)),
    };

    let id = query
        .get("id")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| Some(surface.name.clone()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| format!("component-{}", surface.sha256));

    if blob::put(BIN, &id, &bytes, "application/wasm").is_err() {
        return Outcome::Err(500, "could not store the component bytes".into());
    }
    let doc = json!({
        "id": id, "surface": surface_json(&surface), "uploaded": now(),
    });
    // Re-uploading the same id replaces it, so `just seed-studio` is repeatable.
    let existing = find_one(COMPONENTS, "id", &id);
    let stored = match existing {
        Some((rec_id, revision, _)) => {
            records::update(COMPONENTS, &rec_id, &doc.to_string(), revision).is_ok()
        }
        None => records::create(COMPONENTS, &doc.to_string(), &["id".to_string()]).is_ok(),
    };
    if !stored {
        return Outcome::Err(500, "could not store the surface".into());
    }
    Outcome::Json(201, doc.to_string())
}

fn components_list() -> Outcome {
    let list: Vec<Value> = records::list_records(COMPONENTS, 500, "")
        .map(|p| p.entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| serde_json::from_str::<Value>(&e.data).ok())
        .collect();
    Outcome::Json(200, json!({ "components": list }).to_string())
}

fn component_delete(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let id = b["id"].as_str().unwrap_or_default();
    match find_one(COMPONENTS, "id", id) {
        Some((rec_id, _, _)) => {
            let _ = records::delete(COMPONENTS, &rec_id);
            let _ = blob::delete(BIN, id);
            Outcome::Json(200, json!({ "ok": true, "id": id }).to_string())
        }
        None => Outcome::Err(404, "not_found".into()),
    }
}

fn find_one(coll: &str, field: &str, value: &str) -> Option<(String, u64, Value)> {
    records::find_by(coll, field, &json!(value).to_string())
        .ok()?
        .into_iter()
        .next()
        .and_then(|e| serde_json::from_str::<Value>(&e.data).ok().map(|v| (e.id, e.revision, v)))
}

fn stored_surface(id: &str) -> Option<inspector::Surface> {
    find_one(COMPONENTS, "id", id).map(|(_, _, doc)| surface_from(&doc["surface"]))
}

// ---- resolving a request's graph --------------------------------------------

/// The canvas as the component wants it: nodes rehydrated from storage, edges
/// verbatim. Missing ids are reported rather than silently dropped — a plan over
/// a partially-resolved graph would be a lie.
fn resolve(b: &Value) -> Result<(Vec<composer::Node>, Vec<composer::Edge>), Outcome> {
    let ids: Vec<String> = b["nodes"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| {
                    v.as_str().map(String::from).or_else(|| v["id"].as_str().map(String::from))
                })
                .collect()
        })
        .unwrap_or_default();
    if ids.is_empty() {
        return Err(Outcome::Err(422, "nodes required".into()));
    }
    let mut nodes = Vec::new();
    let mut missing = Vec::new();
    for id in &ids {
        match stored_surface(id) {
            Some(surface) => nodes.push(composer::Node { id: id.clone(), surface }),
            None => missing.push(id.clone()),
        }
    }
    if !missing.is_empty() {
        return Err(Outcome::Err(
            422,
            format!("unknown component(s): {} — upload them first", missing.join(", ")),
        ));
    }
    let edges: Vec<composer::Edge> = b["edges"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|e| composer::Edge {
                    plug: e["plug"].as_str().unwrap_or_default().to_string(),
                    socket: e["socket"].as_str().unwrap_or_default().to_string(),
                    iface: e["iface"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok((nodes, edges))
}

fn plan_json(p: &composer::CompositionPlan) -> Value {
    json!({
        "steps": p.steps.iter().map(|s| json!({
            "order": s.order, "socket": s.socket, "plugs": s.plugs,
            "output": s.output, "also_satisfies": s.also_satisfies
        })).collect::<Vec<_>>(),
        "unsatisfied": p.unsatisfied.iter().map(|g| json!({
            "node": g.node, "iface": g.iface.raw, "name": g.iface.name
        })).collect::<Vec<_>>(),
        "host_needs": p.host_needs.iter().map(|h| json!({
            "raw": h.raw, "namespace": h.namespace, "pkg": h.pkg, "name": h.name
        })).collect::<Vec<_>>(),
        "cyclic": p.cyclic,
        "instance_count": p.instance_count,
        "over_instance_limit": p.over_instance_limit,
        "depth": p.depth.iter().map(|(id, d)| json!({ "id": id, "depth": d })).collect::<Vec<_>>(),
        "roots": p.roots,
        "problems": p.problems.iter().map(|pr| json!({ "kind": pr.kind, "detail": pr.detail })).collect::<Vec<_>>(),
        "buildable": !p.cyclic && p.problems.iter().all(|pr| pr.kind != "cycle"),
    })
}

fn plan_route(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (nodes, edges) = match resolve(&b) {
        Ok(pair) => pair,
        Err(o) => return o,
    };
    let plan = composer::plan(&nodes, &edges);
    Outcome::Json(200, plan_json(&plan).to_string())
}

/// The UI's connection guard: which interfaces `wac` would actually wire between
/// these two. An empty list means the edge must not be drawn.
fn satisfies_route(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (socket_id, plug_id) = (
        b["socket"].as_str().unwrap_or_default(),
        b["plug"].as_str().unwrap_or_default(),
    );
    let (Ok(socket), Ok(plug)) = (blob::get(BIN, socket_id), blob::get(BIN, plug_id)) else {
        return Outcome::Err(404, "socket or plug bytes not stored".into());
    };
    match composer::satisfies(&socket, &plug) {
        Ok(ifaces) => Outcome::Json(
            200,
            json!({ "socket": socket_id, "plug": plug_id, "interfaces": ifaces }).to_string(),
        ),
        Err(e) => Outcome::Err(422, reflect_error(&e)),
    }
}

// ---- the three text forms ---------------------------------------------------

fn emit_route(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (nodes, edges) = match resolve(&b) {
        Ok(pair) => pair,
        Err(o) => return o,
    };
    let plan = composer::plan(&nodes, &edges);
    let meta = &b["meta"];
    let name = meta["name"].as_str().unwrap_or("app").to_string();

    let (text, content_type) = match b["form"].as_str().unwrap_or("plug") {
        "plug" => (
            composer::emit_plug_script(&plan, meta["out_dir"].as_str().unwrap_or("components/target")),
            "text/x-shellscript; charset=utf-8",
        ),
        "wac" => (
            composer::emit_wac(&nodes, &edges, &plan, &format!("{name}:composed")),
            "text/plain; charset=utf-8",
        ),
        "workload" => {
            let m = composer::WorkloadMeta {
                name: name.clone(),
                namespace: meta["namespace"].as_str().unwrap_or(&name).to_string(),
                registry: meta["registry"].as_str().unwrap_or_default().to_string(),
                tag: meta["tag"].as_str().unwrap_or("0.1.0").to_string(),
                replicas: meta["replicas"].as_u64().unwrap_or(1) as u32,
                pool_size: meta["pool_size"].as_u64().unwrap_or(8) as u32,
                max_invocations: meta["max_invocations"].as_u64().unwrap_or(200) as u32,
                http_host: meta["http_host"].as_str().unwrap_or_default().to_string(),
            };
            (composer::emit_workload(&nodes, &plan, &m), "text/yaml; charset=utf-8")
        }
        other => return Outcome::Err(422, format!("unknown form `{other}` (plug|wac|workload)")),
    };
    raw(200, content_type, text.into_bytes())
}

// ---- composing for real -----------------------------------------------------

fn compose_route(request: &IncomingRequest) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let (nodes, edges) = match resolve(&b) {
        Ok(pair) => pair,
        Err(o) => return o,
    };
    let plan = composer::plan(&nodes, &edges);

    // Pick the root: the caller's, or the only node nothing plugs into.
    let root = match b["root"].as_str().filter(|s| !s.is_empty()) {
        Some(r) => r.to_string(),
        None => match plan.roots.as_slice() {
            [only] => only.clone(),
            [] => return Outcome::Err(422, "no root: every component has something plugged into it (a cycle?)".into()),
            many => {
                return Outcome::Err(
                    422,
                    format!("ambiguous root — pass one of: {}", many.join(", ")),
                )
            }
        },
    };

    let mut parts = Vec::new();
    for node in &nodes {
        match blob::get(BIN, &node.id) {
            Ok(bytes) => parts.push(composer::Part { id: node.id.clone(), bytes }),
            Err(_) => {
                return Outcome::Err(422, format!("no stored bytes for `{}`", node.id))
            }
        }
    }

    match composer::compose(&parts, &edges, &root) {
        Ok(bytes) => {
            // Reflect the result so the caller learns what it still imports —
            // the composed artifact is only useful with its remaining host needs.
            let left = inspector::inspect(&bytes)
                .map(|s| {
                    s.host_imports.iter().map(|h| h.raw.clone()).collect::<Vec<_>>().join(",")
                })
                .unwrap_or_default();
            let headers = vec![
                ("content-type".to_string(), "application/wasm".to_string()),
                (
                    "content-disposition".to_string(),
                    format!("attachment; filename=\"{}.composed.wasm\"", root.replace('-', "_")),
                ),
                ("x-studio-host-imports".to_string(), left),
                ("x-studio-instances".to_string(), plan.instance_count.to_string()),
            ];
            Outcome::Raw(200, headers, bytes)
        }
        Err(e) => {
            let (kind, detail) = match e {
                composer::ComposeError::MissingPart(m) => ("missing_part", m),
                composer::ComposeError::Unbuildable(m) => ("unbuildable", m),
                composer::ComposeError::PlugFailed(m) => ("plug_failed", m),
                composer::ComposeError::EncodeFailed(m) => ("encode_failed", m),
            };
            Outcome::Json(422, json!({ "error": kind, "detail": detail }).to_string())
        }
    }
}

// ---- saved canvases ---------------------------------------------------------

fn graph_save(request: &IncomingRequest, id: Option<&str>) -> Outcome {
    let b = match body(request) {
        Ok(v) => v,
        Err(o) => return o,
    };
    let doc = json!({
        "name": b["name"].as_str().unwrap_or("untitled"),
        // The canvas verbatim: node positions live here, the studio never
        // interprets them (that is the UI's business).
        "nodes": b["nodes"].clone(),
        "edges": b["edges"].clone(),
        "saved": now(),
    });
    match id {
        Some(id) => match records::get(GRAPHS, id) {
            Ok(existing) => {
                match records::update(GRAPHS, id, &doc.to_string(), existing.revision) {
                    Ok(_) => Outcome::Json(200, json!({ "id": id }).to_string()),
                    Err(_) => Outcome::Err(409, "revision conflict — reload".into()),
                }
            }
            Err(_) => Outcome::Err(404, "not_found".into()),
        },
        None => match records::create(GRAPHS, &doc.to_string(), &[]) {
            Ok(rec) => Outcome::Json(201, json!({ "id": rec.id }).to_string()),
            Err(_) => Outcome::Err(500, "could not save".into()),
        },
    }
}

fn graph_get(id: &str) -> Outcome {
    match records::get(GRAPHS, id) {
        Ok(e) => {
            let mut v: Value = serde_json::from_str(&e.data).unwrap_or_else(|_| json!({}));
            v["id"] = json!(id);
            Outcome::Json(200, v.to_string())
        }
        Err(_) => Outcome::Err(404, "not_found".into()),
    }
}

fn graphs_list() -> Outcome {
    let list: Vec<Value> = records::list_records(GRAPHS, 200, "")
        .map(|p| p.entries)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|e| {
            serde_json::from_str::<Value>(&e.data).ok().map(|mut v| {
                v["id"] = json!(e.id);
                // The list view wants names and sizes, not the whole canvas.
                json!({
                    "id": e.id,
                    "name": v["name"],
                    "saved": v["saved"],
                    "nodes": v["nodes"].as_array().map(|a| a.len()).unwrap_or(0),
                    "edges": v["edges"].as_array().map(|a| a.len()).unwrap_or(0),
                })
            })
        })
        .collect();
    Outcome::Json(200, json!({ "graphs": list }).to_string())
}

// ---- http plumbing ---------------------------------------------------------

fn reflect_error(e: &inspector::ReflectError) -> String {
    match e {
        inspector::ReflectError::NotAComponent(m) => format!("not a component: {m}"),
        inspector::ReflectError::BadWasm(m) => format!("bad wasm: {m}"),
    }
}

fn body(request: &IncomingRequest) -> Result<Value, Outcome> {
    let raw = read_body(request).map_err(|_| Outcome::Err(400, "could not read body".into()))?;
    if raw.is_empty() {
        return Ok(Value::Object(Map::new()));
    }
    serde_json::from_slice(&raw).map_err(|e| Outcome::Err(400, format!("bad json: {e}")))
}

/// Read the whole body. Component uploads are megabytes, so this reads in 64 KiB
/// chunks; a read error is an ERROR, not a short body — the usual pattern in this
/// repo silently truncates, which for a .wasm means a corrupt component that
/// looks fine until it doesn't.
/// The most a request body may be, before the component stops reading it.
///
/// There was no ceiling anywhere: 148 of 150 components accumulated whatever
/// arrived until the guest hit wasmtime's 64 MiB per-store memory cap and TRAPPED,
/// which reaches the caller as a closed connection saying nothing about a size.
/// A component that answers JSON has no business reading sixteen megabytes, and
/// the ones that legitimately handle uploads police it themselves with a 413 and a
/// granted max-size — those are left alone.
///
/// Generous on purpose. This is a backstop against an unbounded read, not a
/// content policy; an API that needs a real limit should state its own and say 413.
const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;

fn read_body(request: &IncomingRequest) -> Result<Vec<u8>, ()> {
    let b = request.consume().map_err(|_| ())?;
    let stream = b.stream().map_err(|_| ())?;
    let mut buf = Vec::new();
    loop {
        match stream.blocking_read(65536) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => {
                // A ceiling, not a policy: past this the read stops and the caller
                // is told, rather than growing until the store's memory cap traps
                // the component and the connection just closes.
                if buf.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(());
                }
                buf.extend_from_slice(&chunk);
            }
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Err(()),
        }
    }
    Ok(buf)
}

fn emit_response(response_out: ResponseOutparam, result: Outcome) {
    let (code, header_pairs, body) = match result {
        Outcome::Json(c, b) => (
            c,
            vec![("content-type".to_string(), "application/json".to_string())],
            b.into_bytes(),
        ),
        Outcome::Raw(c, h, b) => (c, h, b),
        Outcome::Err(c, m) => (
            c,
            vec![("content-type".to_string(), "application/json".to_string())],
            json!({ "error": m }).to_string().into_bytes(),
        ),
    };
    let headers = Fields::new();
    for (k, v) in &header_pairs {
        let _ = headers.set(k, &[v.as_bytes().to_vec()]);
    }
    let response = OutgoingResponse::new(headers);
    let _ = response.set_status_code(code);
    let out = response.body().expect("outgoing body");
    ResponseOutparam::set(response_out, Ok(response));
    if !body.is_empty() {
        let stream = out.write().expect("write stream");
        for chunk in body.chunks(4096) {
            let _ = stream.blocking_write_and_flush(chunk);
        }
    }
    let _ = OutgoingBody::finish(out, None);
}

bindings::export!(Component with_types_in bindings);
