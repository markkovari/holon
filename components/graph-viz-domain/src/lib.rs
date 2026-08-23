//! `graph-viz-domain` — draw the capability graph as a picture you can pan and read

#[allow(warnings)]
mod bindings;

use bindings::exports::wasi::http::incoming_handler::Guest;
use bindings::knowledge::graph::store::{self};
use bindings::wasi::http::types::{
    IncomingRequest, Method, OutgoingBody, OutgoingResponse, ResponseOutparam,
};
use serde_json::json;

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
    let html = r##"<!DOCTYPE html>
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
        .legend { position: absolute; bottom: 20px; left: 20px; background: rgba(30, 41, 59, 0.9); padding: 1rem; border-radius: 8px; border: 1px solid #475569; z-index: 10; font-size: 0.85rem; backdrop-filter: blur(4px); }
        .legend-item { display: flex; align-items: center; gap: 0.5rem; margin-bottom: 0.4rem; cursor: pointer; user-select: none; transition: opacity 0.2s; }
        .legend-item:hover { opacity: 0.8; }
        .legend-item.disabled { opacity: 0.3; }
        .legend-item:last-child { margin-bottom: 0; }
        .legend-color { width: 16px; height: 16px; border-radius: 4px; }
        .shape-star { clip-path: polygon(50% 0%, 61% 35%, 98% 35%, 68% 57%, 79% 91%, 50% 70%, 21% 91%, 32% 57%, 2% 35%, 39% 35%); }
        .shape-diamond { clip-path: polygon(50% 0%, 100% 50%, 50% 100%, 0% 50%); border-radius: 0; }
        .shape-round-rect { border-radius: 6px; }
        #searchInput { background: #1e293b; border: 1px solid #475569; color: #f8fafc; padding: 0.4rem 0.8rem; border-radius: 4px; font-size: 0.85rem; width: 200px; transition: border-color 0.2s; }
        #searchInput:focus { outline: none; border-color: #3b82f6; }
    </style>
</head>
<body>
    <header>
        <h1>
            <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M18 3a3 3 0 0 0-3 3v12a3 3 0 0 0 3 3 3 3 0 0 0 3-3 3 3 0 0 0-3-3H6a3 3 0 0 0-3 3 3 3 0 0 0 3 3 3 3 0 0 0 3-3V6a3 3 0 0 0-3-3 3 3 0 0 0-3 3 3 3 0 0 0 3 3h12a3 3 0 0 0 3-3 3 3 0 0 0-3-3z"></path></svg>
            Holon <span>Graph Visualizer</span>
        </h1>
        <div class="controls">
            <input type="text" id="searchInput" placeholder="Search nodes..." autocomplete="off">
            <label class="auto-refresh">
                <input type="checkbox" id="autoRefresh" checked> Auto-refresh (5s)
            </label>
            <button class="btn-primary" onclick="refreshGraph()">Force Refresh</button>
        </div>
    </header>
    
    <div id="cy"></div>
    
    <div class="legend">
        <div class="panel-title" style="font-size:0.95rem;">Legend</div>
        <div class="legend-item" data-kind="app"><div class="legend-color shape-star" style="background:#0ea5e9;"></div> App (Star)</div>
        <div class="legend-item" data-kind="artifact"><div class="legend-color shape-round-rect" style="background:#10b981;"></div> Component (Round Rect)</div>
        <div class="legend-item" data-kind="interface"><div class="legend-color shape-diamond" style="background:#8b5cf6;"></div> Interface (Diamond)</div>
        <div class="legend-item" data-kind="goal"><div class="legend-color" style="background:#f59e0b; border-radius:50%"></div> Goal</div>
        <div class="legend-item" data-kind="generation"><div class="legend-color" style="background:#3b82f6; border-radius:50%"></div> Generation</div>
        <div class="legend-item" data-kind="verdict"><div class="legend-color" style="background:#ef4444; border-radius:50%"></div> Verdict</div>
    </div>
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
                'artifact': '#10b981', // Components
                'app': '#0ea5e9',      // Apps
                'interface': '#8b5cf6',
                'knowledge': '#f472b6',
                'file': '#94a3b8'
            };
            return colors[kind] || '#64748b';
        }

        function getShapeForKind(kind) {
            if (kind === 'app') return 'star';
            if (kind === 'interface') return 'diamond';
            if (kind === 'artifact') return 'round-rectangle';
            if (kind === 'goal') return 'hexagon';
            return 'ellipse';
        }

        async function refreshGraph() {
            try {
                const data = await fetchGraphData();
                
                if (!cy) {
                    cy = cytoscape({
                        container: document.getElementById('cy'),
                        elements: data,
                        autoungrabify: true,
                        style: [
                            {
                                selector: 'node',
                                style: {
                                    'label': 'data(label)',
                                    'background-color': function(ele) { return getColorForKind(ele.data('kind')); },
                                    'shape': function(ele) { return getShapeForKind(ele.data('kind')); },
                                    'color': '#e2e8f0',
                                    'text-valign': 'bottom',
                                    'text-halign': 'center',
                                    'text-margin-y': 6,
                                    'font-size': '12px',
                                    'font-weight': 'bold',
                                    'text-outline-color': '#0f172a',
                                    'text-outline-width': 2,
                                    'width': function(ele) { return ele.data('kind') === 'app' ? '60px' : '40px'; },
                                    'height': function(ele) { return ele.data('kind') === 'app' ? '60px' : '40px'; },
                                    'text-wrap': 'ellipsis',
                                    'text-max-width': '120px'
                                }
                            },
                            {
                                selector: 'node[kind="interface"]',
                                style: {
                                    'width': '30px',
                                    'height': '30px'
                                }
                            },
                            {
                                selector: 'edge',
                                style: {
                                    'width': 1.5,
                                    'line-color': '#334155',
                                    'target-arrow-color': '#334155',
                                    'target-arrow-shape': 'triangle',
                                    'curve-style': 'bezier',
                                    'font-size': '10px',
                                    'color': '#94a3b8',
                                    'text-rotation': 'autorotate',
                                    'text-outline-width': 2,
                                    'text-outline-color': '#0f172a',
                                    'arrow-scale': 1.2
                                }
                            },
                            {
                                selector: '.faded',
                                style: {
                                    'opacity': 0.1,
                                    'text-opacity': 0
                                }
                            },
                            {
                                selector: 'node.highlighted',
                                style: {
                                    'border-width': 4,
                                    'border-color': '#f8fafc',
                                    'opacity': 1,
                                    'z-index': 9999
                                }
                            },
                            {
                                selector: 'edge.highlighted',
                                style: {
                                    'width': 3,
                                    'line-color': '#94a3b8',
                                    'target-arrow-color': '#94a3b8',
                                    'opacity': 1,
                                    'z-index': 9999
                                }
                            },
                            {
                                selector: '.hidden-node',
                                style: {
                                    'display': 'none'
                                }
                            }
                        ],
                        layout: {
                            name: 'concentric',
                            concentric: function(node) { return node.degree(); },
                            levelWidth: function(nodes) { return 3; },
                            padding: 50,
                            spacingFactor: 1.2,
                            animate: true
                        }
                    });

                    cy.on('tap', 'node', function(evt){
                        const node = evt.target;
                        
                        // Clear search field when a node is clicked
                        document.getElementById('searchInput').value = '';
                        
                        // Highlight logic
                        cy.elements().removeClass('highlighted faded');
                        
                        const neighborhood = node.neighborhood();
                        cy.elements().addClass('faded');
                        node.removeClass('faded').addClass('highlighted');
                        neighborhood.removeClass('faded').addClass('highlighted');

                        showNodePanel(node);
                    });
                    
                    cy.on('tap', function(evt){
                        if(evt.target === cy){
                            document.getElementById('node-panel').style.display = 'none';
                            cy.elements().removeClass('highlighted faded');
                        }
                    });

                } else {
                    // Differential update
                    const existingIds = new Set(cy.elements().map(e => e.id()));
                    const incomingIds = new Set();
                    const toAdd = [];
                    
                    for (const node of data.nodes) {
                        incomingIds.add(node.data.id);
                        if (!existingIds.has(node.data.id)) {
                            toAdd.push(node);
                        }
                    }
                    for (const edge of data.edges) {
                        incomingIds.add(edge.data.id);
                        if (!existingIds.has(edge.data.id)) {
                            toAdd.push(edge);
                        }
                    }
                    
                    const toRemove = cy.elements().filter(ele => !incomingIds.has(ele.id()));
                    
                    let changed = false;
                    if (toRemove.length > 0) {
                        cy.remove(toRemove);
                        changed = true;
                    }
                    if (toAdd.length > 0) {
                        const newEles = cy.add(toAdd);
                        newEles.forEach(ele => {
                            if (ele.isNode() && hiddenKinds.has(ele.data('kind'))) {
                                ele.addClass('hidden-node');
                            }
                        });
                        changed = true;
                    }

                    if (changed) {
                        cy.layout({
                            name: 'concentric',
                            concentric: function(node) { return node.degree(); },
                            levelWidth: function(nodes) { return 3; },
                            padding: 50,
                            spacingFactor: 1.2,
                            animate: true
                        }).run();
                    }
                }
            } catch (err) {
                console.error("Failed to fetch graph data", err);
            }
        }

        function showNodePanel(node) {
            const data = node.data();
            const panel = document.getElementById('node-panel');
            panel.style.display = 'block';
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

            // List connected elements
            const connectedNodes = node.neighborhood('node');
            if (connectedNodes.length > 0) {
                html += `<div class="panel-prop" style="margin-top: 15px; padding-top: 10px; border-top: 1px solid #334155;"><strong>Connected Elements:</strong></div>`;
                html += `<ul style="padding-left: 20px; margin-top: 5px; font-size: 0.85rem;">`;
                
                connectedNodes.forEach(n => {
                    const nData = n.data();
                    const label = nData.label || nData.id;
                    const kind = nData.kind || '';
                    html += `<li style="margin-bottom: 4px;"><a href="#" class="connected-link" data-id="${nData.id}" style="color: #60a5fa; text-decoration: none;">[${kind}] ${label}</a></li>`;
                });
                
                html += `</ul>`;
            }

            document.getElementById('panelContent').innerHTML = html;

            // Add click listeners to links
            document.querySelectorAll('.connected-link').forEach(link => {
                link.addEventListener('click', (e) => {
                    e.preventDefault();
                    const targetId = e.currentTarget.getAttribute('data-id');
                    const targetNode = cy.getElementById(targetId);
                    if (targetNode.length > 0) {
                        targetNode.emit('tap');
                        cy.center(targetNode);
                    }
                });
            });
        }

        document.getElementById('autoRefresh').addEventListener('change', (e) => {
            if (e.target.checked) {
                refreshInterval = setInterval(refreshGraph, 5000);
            } else {
                clearInterval(refreshInterval);
            }
        });

        let hiddenKinds = new Set();
        document.querySelectorAll('.legend-item').forEach(item => {
            item.addEventListener('click', () => {
                const kind = item.getAttribute('data-kind');
                if (!kind) return;
                
                if (hiddenKinds.has(kind)) {
                    hiddenKinds.delete(kind);
                    item.classList.remove('disabled');
                } else {
                    hiddenKinds.add(kind);
                    item.classList.add('disabled');
                }
                
                if (cy) {
                    cy.batch(() => {
                        cy.nodes().forEach(node => {
                            if (hiddenKinds.has(node.data('kind'))) {
                                node.addClass('hidden-node');
                            } else {
                                node.removeClass('hidden-node');
                            }
                        });
                    });
                }
            });
        });

        document.getElementById('searchInput').addEventListener('input', (e) => {
            if (!cy) return;
            const query = e.target.value.toLowerCase().trim();
            document.getElementById('node-panel').style.display = 'none';
            
            cy.batch(() => {
                cy.elements().removeClass('highlighted faded');
                
                if (!query) {
                    // Re-apply legend hidden state
                    cy.nodes().forEach(node => {
                        if (hiddenKinds.has(node.data('kind'))) {
                            node.addClass('hidden-node');
                        }
                    });
                    return;
                }
                
                cy.elements().addClass('faded');
                
                const matches = cy.nodes().filter(node => {
                    if (hiddenKinds.has(node.data('kind'))) return false;
                    const label = (node.data('label') || '').toLowerCase();
                    const id = (node.data('id') || '').toLowerCase();
                    return label.includes(query) || id.includes(query);
                });
                
                matches.removeClass('faded hidden-node').addClass('highlighted');
            });
        });

        // Initial load
        refreshGraph();
        refreshInterval = setInterval(refreshGraph, 5000);
    </script>
</body>
</html>"##;
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
                let _ = headers.set("content-type", &[b"text/html".to_vec()]);
                let resp = OutgoingResponse::new(headers);
                let _ = resp.set_status_code(200);
                let out = resp.body().expect("body");
                ResponseOutparam::set(response_out, Ok(resp));
                if let Ok(stream) = out.write() {
                    let mut bytes = html.as_bytes();
                    while !bytes.is_empty() {
                        let ready = match stream.check_write() {
                            Ok(0) => {
                                stream.subscribe().block();
                                continue;
                            }
                            Ok(n) => n as usize,
                            Err(_) => break,
                        };
                        let take = ready.min(bytes.len());
                        if stream.write(&bytes[..take]).is_err() {
                            break;
                        }
                        bytes = &bytes[take..];
                    }
                    let _ = stream.blocking_flush();
                    drop(stream);
                }
                let _ = OutgoingBody::finish(out, None);
            }
            Outcome::Json(code, json) => {
                let headers = bindings::wasi::http::types::Fields::new();
                let _ = headers.set("content-type", &[b"application/json".to_vec()]);
                let resp = OutgoingResponse::new(headers);
                let _ = resp.set_status_code(code);
                let out = resp.body().expect("body");
                ResponseOutparam::set(response_out, Ok(resp));
                if let Ok(stream) = out.write() {
                    let mut bytes = json.as_bytes();
                    while !bytes.is_empty() {
                        let ready = match stream.check_write() {
                            Ok(0) => {
                                stream.subscribe().block();
                                continue;
                            }
                            Ok(n) => n as usize,
                            Err(_) => break,
                        };
                        let take = ready.min(bytes.len());
                        if stream.write(&bytes[..take]).is_err() {
                            break;
                        }
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
                let _ = headers.set("content-type", &[b"application/json".to_vec()]);
                let resp = OutgoingResponse::new(headers);
                let _ = resp.set_status_code(code);
                let out = resp.body().expect("body");
                ResponseOutparam::set(response_out, Ok(resp));
                if let Ok(stream) = out.write() {
                    let _ = write_all(&stream, json.as_bytes());
                    drop(stream);
                }
                let _ = OutgoingBody::finish(out, None);
            }
        }
    }
}

bindings::export!(Component with_types_in bindings);

/// Write every byte, respecting what the stream says it can take.
///
/// `blocking_write_and_flush` accepts at most 4096 bytes and TRAPS above it,
/// which kills the component mid-response — the caller sees a closed connection
/// and no status. Any page or JSON body larger than 4 KiB hits it, so the size
/// of the payload decides whether the endpoint works.
///
/// `check_write` reports what the stream will accept now; a zero means block on
/// the pollable and ask again. Copied from the shape every other domain here
/// already uses.
fn write_all(stream: &bindings::wasi::io::streams::OutputStream, mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        let ready = match stream.check_write() {
            Ok(0) => {
                stream.subscribe().block();
                continue;
            }
            Ok(n) => n as usize,
            Err(_) => return false,
        };
        let take = ready.min(bytes.len());
        if stream.write(&bytes[..take]).is_err() {
            return false;
        }
        bytes = &bytes[take..];
    }
    stream.blocking_flush().is_ok()
}
