#[allow(warnings)]
mod bindings;

use bindings::knowledge::graph::store::{self, Node, Direction};
use bindings::wasi::http::types::{
    IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use bindings::exports::wasi::http::incoming_handler::Guest;
use serde_json::{json, Value};

struct Component;

enum Outcome {
    Html(String),
    Json(u16, String),
    Error(u16, String),
}

fn handle_query(req: IncomingRequest) -> Outcome {
    let Ok(body) = req.consume() else {
        return Outcome::Error(400, "could not consume body".into());
    };
    let Ok(stream) = body.stream() else {
        return Outcome::Error(400, "could not get stream".into());
    };
    let mut out = Vec::new();
    loop {
        match stream.blocking_read(64 * 1024) {
            Ok(chunk) if chunk.is_empty() => break,
            Ok(chunk) => out.extend_from_slice(&chunk),
            Err(bindings::wasi::io::streams::StreamError::Closed) => break,
            Err(_) => return Outcome::Error(500, "read error".into()),
        }
    }
    let surql = String::from_utf8_lossy(&out).into_owned();

    match store::query(&surql) {
        Ok(result) => Outcome::Json(200, result),
        Err(e) => {
            let msg = match e {
                store::GraphError::Rejected(m) => m,
                store::GraphError::Unavailable(m) => m,
                store::GraphError::NotConfigured(m) => m,
            };
            Outcome::Error(500, msg)
        }
    }
}

fn serve_ui() -> Outcome {
    let html = r#"<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <title>Holon Graph Visualizer</title>
    <script src="https://cdnjs.cloudflare.com/ajax/libs/cytoscape/3.26.0/cytoscape.min.js"></script>
    <style>
        body { margin: 0; padding: 0; font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Helvetica, Arial, sans-serif; background: #0f172a; color: #e2e8f0; display: flex; flex-direction: column; height: 100vh; overflow: hidden; }
        header { background: #1e293b; padding: 1rem 1.5rem; border-bottom: 1px solid #334155; display: flex; justify-content: space-between; align-items: center; z-index: 10; }
        h1 { margin: 0; font-size: 1.25rem; font-weight: 700; color: #f8fafc; display: flex; align-items: center; gap: 0.5rem; }
        h1 span { font-size: 0.85rem; color: #94a3b8; font-weight: 400; }
        #cy { flex: 1; width: 100%; position: relative; z-index: 1; }
        .controls { display: flex; gap: 0.5rem; align-items: center; }
        select, button { background: #334155; border: 1px solid #475569; color: #f8fafc; padding: 0.4rem 0.8rem; border-radius: 4px; font-size: 0.85rem; cursor: pointer; }
        button:hover { background: #475569; }
        .btn-primary { background: #3b82f6; border-color: #2563eb; font-weight: 600; }
        .btn-primary:hover { background: #2563eb; }
        #node-panel { position: absolute; top: 80px; right: 20px; width: 300px; background: rgba(30, 41, 59, 0.9); border: 1px solid #475569; border-radius: 8px; padding: 1rem; box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1); z-index: 20; display: none; max-height: calc(100vh - 120px); overflow-y: auto; backdrop-filter: blur(4px); }
        .panel-title { font-weight: 700; color: #f8fafc; margin-bottom: 0.5rem; font-size: 1.1rem; border-bottom: 1px solid #334155; padding-bottom: 0.5rem; }
        .panel-prop { font-size: 0.85rem; margin-bottom: 0.4rem; word-break: break-all; }
        .panel-prop strong { color: #94a3b8; }
        .auto-refresh { display: flex; align-items: center; gap: 0.4rem; font-size: 0.85rem; color: #cbd5e1; }
        pre { background: #0f172a; padding: 0.5rem; border-radius: 4px; border: 1px solid #334155; white-space: pre-wrap; font-size: 0.75rem; color: #a5b4fc; }
    </style>
</head>
<body>
    <header>
        <h1>Holon <span>Graph Visualizer</span></h1>
        <div class="controls">
            <label class="auto-refresh">
                <input type="checkbox" id="autoRefresh" checked> Auto-refresh (5s)
            </label>
            <button onclick="refreshGraph()" class="btn-primary">Force Refresh</button>
        </div>
    </header>
    <div id="cy"></div>
    <div id="node-panel">
        <div class="panel-title" id="panelTitle">Node Details</div>
        <div id="panelContent"></div>
    </div>

    <script>
        let cy;
        let refreshInterval;

        async function query(surql) {
            const res = await fetch('/api/query', {
                method: 'POST',
                body: surql
            });
            return await res.json();
        }

        async function fetchGraphData() {
            // First, get all tables to see what exists
            const infoRes = await query('INFO FOR DB;');
            const tables = Object.keys(infoRes[0]?.result?.tables || {});
            
            if (tables.length === 0) return { nodes: [], edges: [] };

            // Query all records from all tables
            const selectQueries = tables.map(t => `SELECT * FROM type::table('${t}');`).join('\n');
            const results = await query(selectQueries);
            
            const nodes = [];
            const edges = [];
            
            // SurrealDB returns an array of results, one for each statement
            (Array.isArray(results) ? results : [results]).forEach(resultSet => {
                const records = resultSet.result;
                if (!Array.isArray(records)) return;
                
                records.forEach(record => {
                    if (!record.id) return;
                    
                    // Edges in surrealdb have 'in' and 'out' properties
                    if (record.in && record.out) {
                        edges.push({
                            data: {
                                id: record.id,
                                source: record.in,
                                target: record.out,
                                label: record.id.split(':')[0],
                                ...record
                            }
                        });
                    } else {
                        // Regular node
                        nodes.push({
                            data: {
                                id: record.id,
                                label: record.name || record.title || record.id,
                                kind: record.id.split(':')[0],
                                ...record
                            }
                        });
                    }
                });
            });

            return { nodes, edges };
        }

        function getColorForKind(kind) {
            const colors = {
                'goal': '#f59e0b',
                'generation': '#3b82f6',
                'verdict': '#ef4444',
                'evaluation': '#8b5cf6',
                'component': '#10b981',
                'app': '#06b6d4',
                'knowledge': '#f472b6',
                'file': '#94a3b8'
            };
            return colors[kind] || '#64748b';
        }

        async function refreshGraph() {
            try {
                const data = await fetchGraphData();
                
                if (!cy) {
                    cy = cytoscape({
                        container: document.getElementById('cy'),
                        elements: data,
                        style: [
                            {
                                selector: 'node',
                                style: {
                                    'label': 'data(label)',
                                    'background-color': function(ele) { return getColorForKind(ele.data('kind')); },
                                    'color': '#fff',
                                    'text-valign': 'center',
                                    'text-halign': 'center',
                                    'font-size': '10px',
                                    'width': '60px',
                                    'height': '60px',
                                    'text-wrap': 'ellipsis',
                                    'text-max-width': '50px'
                                }
                            },
                            {
                                selector: 'edge',
                                style: {
                                    'width': 2,
                                    'line-color': '#475569',
                                    'target-arrow-color': '#475569',
                                    'target-arrow-shape': 'triangle',
                                    'curve-style': 'bezier',
                                    'label': 'data(label)',
                                    'font-size': '8px',
                                    'color': '#94a3b8',
                                    'text-rotation': 'autorotate'
                                }
                            }
                        ],
                        layout: {
                            name: 'cose',
                            padding: 50,
                            nodeRepulsion: 400000,
                            idealEdgeLength: 100,
                            gravity: 0.8
                        }
                    });

                    cy.on('tap', 'node', function(evt){
                        const node = evt.target;
                        showNodePanel(node.data());
                    });
                    
                    cy.on('tap', function(evt){
                        if(evt.target === cy){
                            document.getElementById('node-panel').style.display = 'none';
                        }
                    });

                } else {
                    // Update existing graph without losing positions if possible
                    cy.elements().remove();
                    cy.add(data);
                    cy.layout({
                        name: 'cose',
                        animate: true,
                        randomize: false,
                        fit: false
                    }).run();
                }
            } catch (err) {
                console.error("Failed to fetch graph data", err);
            }
        }

        function showNodePanel(data) {
            document.getElementById('panelTitle').innerText = data.label || data.id;
            let html = '';
            for (const [key, value] of Object.entries(data)) {
                if (key === 'id' || key === 'label' || key === 'kind') continue;
                if (typeof value === 'object') {
                    html += `<div class="panel-prop"><strong>${key}:</strong><pre>${JSON.stringify(value, null, 2)}</pre></div>`;
                } else {
                    html += `<div class="panel-prop"><strong>${key}:</strong> ${value}</div>`;
                }
            }
            document.getElementById('panelContent').innerHTML = html;
            document.getElementById('node-panel').style.display = 'block';
        }

        document.getElementById('autoRefresh').addEventListener('change', (e) => {
            if (e.target.checked) {
                refreshInterval = setInterval(refreshGraph, 5000);
            } else {
                clearInterval(refreshInterval);
            }
        });

        // Initial load
        refreshGraph();
        refreshInterval = setInterval(refreshGraph, 5000);
    </script>
</body>
</html>"#;
    Outcome::Html(html.to_string())
}

impl Guest for Component {
    fn handle(request: IncomingRequest, response_out: ResponseOutparam) {
        let path = request.path_with_query().unwrap_or_else(|| "/".to_string());
        let method = request.method();

        let outcome = match (&method, path.as_str()) {
            (Method::Get, "/") => serve_ui(),
            (Method::Post, "/api/query") => handle_query(request),
            _ => Outcome::Error(404, "not found".into()),
        };

        match outcome {
            Outcome::Html(html) => {
                let headers = bindings::wasi::http::types::Fields::new();
                let _ = headers.set(&"content-type".to_string(), &[b"text/html".to_vec()]);
                let resp = OutgoingResponse::new(headers);
                let _ = resp.set_status_code(200);
                let out = resp.body().expect("body");
                ResponseOutparam::set(response_out, Ok(resp));
                if let Ok(stream) = out.write() {
                    let mut bytes = html.as_bytes();
                    while !bytes.is_empty() {
                        let ready = match stream.check_write() {
                            Ok(0) => { stream.subscribe().block(); continue; }
                            Ok(n) => n as usize,
                            Err(_) => break,
                        };
                        let take = ready.min(bytes.len());
                        if stream.write(&bytes[..take]).is_err() { break; }
                        bytes = &bytes[take..];
                    }
                    let _ = stream.blocking_flush();
                    drop(stream);
                }
                let _ = OutgoingBody::finish(out, None);
            }
            Outcome::Json(code, json) => {
                let headers = bindings::wasi::http::types::Fields::new();
                let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
                let resp = OutgoingResponse::new(headers);
                let _ = resp.set_status_code(code);
                let out = resp.body().expect("body");
                ResponseOutparam::set(response_out, Ok(resp));
                if let Ok(stream) = out.write() {
                    let mut bytes = json.as_bytes();
                    while !bytes.is_empty() {
                        let ready = match stream.check_write() {
                            Ok(0) => { stream.subscribe().block(); continue; }
                            Ok(n) => n as usize,
                            Err(_) => break,
                        };
                        let take = ready.min(bytes.len());
                        if stream.write(&bytes[..take]).is_err() { break; }
                        bytes = &bytes[take..];
                    }
                    let _ = stream.blocking_flush();
                    drop(stream);
                }
                let _ = OutgoingBody::finish(out, None);
            }
            Outcome::Error(code, msg) => {
                let json = json!({ "error": msg }).to_string();
                let headers = bindings::wasi::http::types::Fields::new();
                let _ = headers.set(&"content-type".to_string(), &[b"application/json".to_vec()]);
                let resp = OutgoingResponse::new(headers);
                let _ = resp.set_status_code(code);
                let out = resp.body().expect("body");
                ResponseOutparam::set(response_out, Ok(resp));
                if let Ok(stream) = out.write() {
                    let _ = stream.blocking_write_and_flush(json.as_bytes());
                    drop(stream);
                }
                let _ = OutgoingBody::finish(out, None);
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);
