//! The day's manifest: `GET /api/manifest` (JSON) and `GET /api/manifest.csv`
//! (`text/csv`, formatted by `csv:codec`). The optional radius filter is
//! `geo:resolve`'s work — bounding box as a pre-filter, then the real distance.

use crate::bindings::csv::codec::codec as csv;
use crate::bindings::geo::resolve::coords as geo;
use crate::bindings::records::store::store as records;
use crate::bindings::wasi::http::types::Method;
use crate::{Reply, Route};
use serde_json::{json, Map, Value};

/// One request, flattened to exactly the fields both answers need.
struct Item {
    id: String,
    title: String,
    state: String,
    engineer: String,
    distance_m: i64,
    lat: f64,
    lon: f64,
}

const PAGE: u32 = 200;

fn load() -> Result<Vec<Item>, Reply> {
    let mut out: Vec<Item> = Vec::new();
    let mut after = String::new();
    loop {
        let page = match records::list_records("requests", PAGE, &after) {
            Ok(p) => p,
            Err(_) => return Err(Reply::err(500, "store_unavailable")),
        };
        for e in page.entries {
            let d: Value = serde_json::from_str(&e.data).unwrap_or_else(|_| json!({}));
            out.push(Item {
                id: e.id,
                title: d["title"].as_str().unwrap_or_default().to_string(),
                state: d["state"].as_str().unwrap_or_default().to_string(),
                engineer: d["engineer"].as_str().unwrap_or_default().to_string(),
                distance_m: d["distance_m"].as_i64().unwrap_or(0),
                lat: d["lat"].as_f64().unwrap_or(0.0),
                lon: d["lon"].as_f64().unwrap_or(0.0),
            });
        }
        // `next` empty, or not moving, means the last page.
        if page.next.is_empty() || page.next == after {
            break;
        }
        after = page.next;
    }
    out.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(out)
}

/// `?near_lat=&near_lon=&within_m=` — all three or none. Present-but-unparsable
/// is `400 invalid`, and the box is computed once, by `geo`, not here.
struct Circle {
    lat: f64,
    lon: f64,
    radius: f64,
    min_lat: f64,
    min_lon: f64,
    max_lat: f64,
    max_lon: f64,
}

fn radius(route: &Route) -> Result<Option<Circle>, Reply> {
    let raw = ["near_lat", "near_lon", "within_m"].map(|k| route.param(k));
    // Any of the three missing means NO filter — not a bad request.
    if raw.iter().any(|s| s.trim().is_empty()) {
        return Ok(None);
    }
    let bad = || Reply::err(400, "invalid");
    let mut n = [0.0f64; 3];
    for (i, s) in raw.iter().enumerate() {
        n[i] = s.trim().parse::<f64>().map_err(|_| bad())?;
        if !n[i].is_finite() {
            return Err(bad());
        }
    }
    let [lat, lon, radius] = n;
    let bb = geo::bounding_box(geo::Point { lat, lon }, radius).map_err(|_| bad())?;
    Ok(Some(Circle {
        lat,
        lon,
        radius,
        min_lat: bb.min_lat,
        min_lon: bb.min_lon,
        max_lat: bb.max_lat,
        max_lon: bb.max_lon,
    }))
}

impl Circle {
    /// Box first (cheap), then the true distance — a box corner is outside the circle.
    fn holds(&self, it: &Item) -> bool {
        let b = geo::Bbox {
            min_lat: self.min_lat,
            min_lon: self.min_lon,
            max_lat: self.max_lat,
            max_lon: self.max_lon,
        };
        let p = geo::Point { lat: it.lat, lon: it.lon };
        if !geo::contains(b, p) {
            return false;
        }
        let c = geo::Point { lat: self.lat, lon: self.lon };
        matches!(geo::distance_meters(c, geo::Point { lat: it.lat, lon: it.lon }), Ok(d) if d <= self.radius)
            && geo::contains(b, geo::Point { lat: it.lat, lon: it.lon })
    }
}

fn bump(m: &mut Map<String, Value>, key: &str) {
    if key.is_empty() {
        return; // unassigned belongs to no engineer
    }
    let n = m.get(key).and_then(Value::as_i64).unwrap_or(0) + 1;
    m.insert(key.to_string(), json!(n));
}

pub fn handle(method: &Method, route: &Route, _body: &str) -> Reply {
    if !matches!(method, Method::Get) {
        return Reply::err(405, "method_not_allowed");
    }
    let circle = match radius(route) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let rows = match load() {
        Ok(v) => v,
        Err(r) => return r,
    };
    let kept: Vec<Item> = match &circle {
        Some(c) => rows.into_iter().filter(|it| c.holds(it)).collect(),
        None => rows,
    };

    let csv_wanted = route.segments.last().map(String::as_str) == Some("manifest.csv");
    if csv_wanted {
        let mut doc = vec![csv::Row {
            fields: ["id", "title", "state", "engineer", "distance_m"]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        }];
        doc.extend(kept.iter().map(|it| csv::Row {
            fields: vec![
                it.id.clone(),
                it.title.clone(),
                it.state.clone(),
                it.engineer.clone(),
                it.distance_m.to_string(),
            ],
        }));
        let text = csv::format(
            &doc,
            &csv::Dialect { delimiter: ",".to_string(), has_header: true, trim: false },
        );
        return Reply::raw(200, "text/csv", text.into_bytes());
    }

    let mut by_state = Map::new();
    let mut by_engineer = Map::new();
    let mut total_distance_m: i64 = 0;
    for it in &kept {
        bump(&mut by_state, &it.state);
        bump(&mut by_engineer, &it.engineer);
        total_distance_m += it.distance_m;
    }
    Reply::json(
        200,
        json!({
            "total": kept.len(),
            "by_state": Value::Object(by_state),
            "by_engineer": Value::Object(by_engineer),
            "total_distance_m": total_distance_m,
        }),
    )
}