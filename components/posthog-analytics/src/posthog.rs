//! Pure HogQL query builder and PostHog API payload/response codec.
//!
//! Kept free of WIT host calls so SQL escaping, HogQL generation, and response
//! parsing are unit-tested natively without a live PostHog server.

use serde_json::{json, Map, Value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prop {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Window {
    pub from_unix: u64,
    pub to_unix: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Filter {
    pub event_name: String,
    pub project: String,
    pub property_filters: Vec<Prop>,
    pub window: Window,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bucket {
    Hour,
    Day,
    Week,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureEvent {
    pub name: String,
    pub distinct_id: String,
    pub project: String,
    pub timestamp_unix: u64,
    pub properties: Vec<Prop>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpQuery {
    pub project: String,
    pub experiment_key: String,
    pub exposure_event: String,
    pub conversion_event: String,
    pub error_event: String,
    pub variant_property: String,
    pub variants: Vec<String>,
    pub window: Window,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SeriesRow {
    pub bucket_unix: u64,
    pub count: u64,
    pub unique_users: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VariantRow {
    pub variant: String,
    pub exposures: u64,
    pub conversions: u64,
    pub errors: u64,
    pub unique_users: u64,
    pub conversion_rate: f64,
    pub error_rate: f64,
}

/// Single-quote escape for HogQL string literals.
pub fn escape_hogql(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\'', "\\'")
}

/// Validate a property key so it cannot break out of `JSONExtractString(properties, '...')`.
pub fn validate_key(key: &str) -> Result<String, String> {
    if key.is_empty() || key.len() > 128 {
        return Err("property key must be 1..=128 chars".into());
    }
    if !key.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '$' | '/')) {
        return Err(format!("invalid property key: {key:?}"));
    }
    Ok(escape_hogql(key))
}

fn where_clauses(f: &Filter) -> Result<String, String> {
    if f.event_name.is_empty() {
        return Err("event_name is required".into());
    }
    let mut parts = vec![format!("event = '{}'", escape_hogql(&f.event_name))];
    if !f.project.is_empty() {
        parts.push(format!(
            "JSONExtractString(properties, 'project') = '{}'",
            escape_hogql(&f.project)
        ));
    }
    if f.window.from_unix > 0 {
        parts.push(format!("timestamp >= toDateTime({})", f.window.from_unix));
    }
    if f.window.to_unix > 0 {
        parts.push(format!("timestamp < toDateTime({})", f.window.to_unix));
    }
    for p in &f.property_filters {
        let k = validate_key(&p.key)?;
        parts.push(format!("JSONExtractString(properties, '{k}') = '{}'", escape_hogql(&p.value)));
    }
    Ok(parts.join(" AND "))
}

pub fn hogql_count(f: &Filter) -> Result<String, String> {
    let w = where_clauses(f)?;
    Ok(format!("SELECT count() FROM events WHERE {w}"))
}

pub fn hogql_unique_users(f: &Filter) -> Result<String, String> {
    let w = where_clauses(f)?;
    Ok(format!("SELECT count(DISTINCT distinct_id) FROM events WHERE {w}"))
}

pub fn hogql_timeseries(f: &Filter, bucket: Bucket) -> Result<String, String> {
    let w = where_clauses(f)?;
    let fn_name = match bucket {
        Bucket::Hour => "toStartOfHour",
        Bucket::Day => "toStartOfDay",
        Bucket::Week => "toStartOfWeek",
    };
    Ok(format!(
        "SELECT toUnixTimestamp({fn_name}(timestamp)) AS b, count(), count(DISTINCT distinct_id) \
         FROM events WHERE {w} GROUP BY b ORDER BY b ASC"
    ))
}

pub fn hogql_breakdown(f: &Filter, property_key: &str, limit: u32) -> Result<String, String> {
    let w = where_clauses(f)?;
    let k = validate_key(property_key)?;
    let lim = limit.clamp(1, 500);
    Ok(format!(
        "SELECT JSONExtractString(properties, '{k}') AS val, count() AS c \
         FROM events WHERE {w} GROUP BY val ORDER BY c DESC LIMIT {lim}"
    ))
}

pub fn hogql_experiment_results(req: &ExpQuery) -> Result<String, String> {
    if req.exposure_event.is_empty() || req.conversion_event.is_empty() {
        return Err("exposure_event and conversion_event are required".into());
    }
    let vprop = validate_key(&req.variant_property)?;
    let mut events = vec![
        format!("'{}'", escape_hogql(&req.exposure_event)),
        format!("'{}'", escape_hogql(&req.conversion_event)),
    ];
    if !req.error_event.is_empty() {
        events.push(format!("'{}'", escape_hogql(&req.error_event)));
    }
    let mut conds = vec![format!("event IN ({})", events.join(", "))];
    if !req.project.is_empty() {
        conds.push(format!(
            "JSONExtractString(properties, 'project') = '{}'",
            escape_hogql(&req.project)
        ));
    }
    if !req.experiment_key.is_empty() {
        conds.push(format!(
            "JSONExtractString(properties, 'experiment_key') = '{}'",
            escape_hogql(&req.experiment_key)
        ));
    }
    if req.window.from_unix > 0 {
        conds.push(format!("timestamp >= toDateTime({})", req.window.from_unix));
    }
    if req.window.to_unix > 0 {
        conds.push(format!("timestamp < toDateTime({})", req.window.to_unix));
    }
    if !req.variants.is_empty() {
        let vlist: Vec<String> =
            req.variants.iter().map(|v| format!("'{}'", escape_hogql(v))).collect();
        conds.push(format!("JSONExtractString(properties, '{vprop}') IN ({})", vlist.join(", ")));
    }

    let exp_ev = escape_hogql(&req.exposure_event);
    let conv_ev = escape_hogql(&req.conversion_event);
    let err_expr = if req.error_event.is_empty() {
        "toUInt64(0)".to_string()
    } else {
        format!("countIf(event = '{}')", escape_hogql(&req.error_event))
    };

    Ok(format!(
        "SELECT JSONExtractString(properties, '{vprop}') AS variant, \
         countIf(event = '{exp_ev}') AS exposures, \
         countIf(event = '{conv_ev}') AS conversions, \
         {err_expr} AS errors, \
         uniqExactIf(distinct_id, event = '{exp_ev}') AS unique_users \
         FROM events WHERE {} GROUP BY variant ORDER BY variant ASC",
        conds.join(" AND ")
    ))
}

pub fn query_body(hogql: &str) -> String {
    json!({
        "query": {
            "kind": "HogQLQuery",
            "query": hogql,
        }
    })
    .to_string()
}

pub fn capture_body(token: &str, ev: &CaptureEvent) -> String {
    let mut props = Map::new();
    if !ev.project.is_empty() {
        props.insert("project".to_string(), json!(ev.project));
    }
    for p in &ev.properties {
        props.insert(p.key.clone(), json!(p.value));
    }
    let mut payload = json!({
        "api_key": token,
        "event": ev.name,
        "distinct_id": ev.distinct_id,
        "properties": props,
    });
    if ev.timestamp_unix > 0 {
        payload["timestamp"] = json!(ev.timestamp_unix);
    }
    payload.to_string()
}

pub fn batch_body(token: &str, events: &[CaptureEvent]) -> String {
    let batch: Vec<Value> = events
        .iter()
        .map(|ev| {
            let mut props = Map::new();
            if !ev.project.is_empty() {
                props.insert("project".to_string(), json!(ev.project));
            }
            for p in &ev.properties {
                props.insert(p.key.clone(), json!(p.value));
            }
            let mut item = json!({
                "event": ev.name,
                "distinct_id": ev.distinct_id,
                "properties": props,
            });
            if ev.timestamp_unix > 0 {
                item["timestamp"] = json!(ev.timestamp_unix);
            }
            item
        })
        .collect();
    json!({ "api_key": token, "batch": batch }).to_string()
}

pub fn identify_body(token: &str, distinct_id: &str, traits: &[Prop]) -> String {
    let mut set = Map::new();
    for t in traits {
        set.insert(t.key.clone(), json!(t.value));
    }
    json!({
        "api_key": token,
        "event": "$identify",
        "distinct_id": distinct_id,
        "properties": { "$set": set },
    })
    .to_string()
}

pub fn parse_single_u64(body: &[u8]) -> Result<u64, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad json: {e}"))?;
    let n = v["results"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.as_array())
        .and_then(|cols| cols.first())
        .and_then(Value::as_u64)
        .unwrap_or(0);
    Ok(n)
}

pub fn parse_timeseries(body: &[u8]) -> Result<Vec<SeriesRow>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad json: {e}"))?;
    let Some(rows) = v["results"].as_array() else {
        return Ok(Vec::new());
    };
    Ok(rows
        .iter()
        .filter_map(|r| {
            let cols = r.as_array()?;
            Some(SeriesRow {
                bucket_unix: cols.first()?.as_u64()?,
                count: cols.get(1)?.as_u64()?,
                unique_users: cols.get(2)?.as_u64().unwrap_or(0),
            })
        })
        .collect())
}

pub fn parse_breakdown(body: &[u8]) -> Result<Vec<(String, u64)>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad json: {e}"))?;
    let Some(rows) = v["results"].as_array() else {
        return Ok(Vec::new());
    };
    Ok(rows
        .iter()
        .filter_map(|r| {
            let cols = r.as_array()?;
            Some((cols.first()?.as_str()?.to_string(), cols.get(1)?.as_u64()?))
        })
        .collect())
}

pub fn parse_experiment_results(body: &[u8]) -> Result<Vec<VariantRow>, String> {
    let v: Value = serde_json::from_slice(body).map_err(|e| format!("bad json: {e}"))?;
    let Some(rows) = v["results"].as_array() else {
        return Ok(Vec::new());
    };
    Ok(rows
        .iter()
        .filter_map(|r| {
            let cols = r.as_array()?;
            let variant = cols.first()?.as_str()?.to_string();
            let exposures = cols.get(1)?.as_u64().unwrap_or(0);
            let conversions = cols.get(2)?.as_u64().unwrap_or(0);
            let errors = cols.get(3)?.as_u64().unwrap_or(0);
            let unique_users = cols.get(4)?.as_u64().unwrap_or(0);
            let conversion_rate =
                if exposures == 0 { 0.0 } else { conversions as f64 / exposures as f64 };
            let error_rate = if exposures == 0 { 0.0 } else { errors as f64 / exposures as f64 };
            Some(VariantRow {
                variant,
                exposures,
                conversions,
                errors,
                unique_users,
                conversion_rate,
                error_rate,
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hogql_experiment_query_escapes_and_filters_safely() {
        let q = hogql_experiment_results(&ExpQuery {
            project: "pet-clinic".into(),
            experiment_key: "senior-triage-v3".into(),
            exposure_event: "triage_served".into(),
            conversion_event: "visit_booked".into(),
            error_event: "triage_error".into(),
            variant_property: "variant_id".into(),
            variants: vec!["triage-v1-mbt".into(), "triage-v3-clinical".into()],
            window: Window { from_unix: 1700000000, to_unix: 1700086400 },
        })
        .unwrap();
        assert!(q.contains("countIf(event = 'triage_served') AS exposures"));
        assert!(q.contains("countIf(event = 'visit_booked') AS conversions"));
        assert!(q.contains("countIf(event = 'triage_error') AS errors"));
        assert!(q.contains("JSONExtractString(properties, 'project') = 'pet-clinic'"));
        assert!(q.contains("toDateTime(1700000000)"));
    }

    #[test]
    fn invalid_property_key_is_rejected() {
        assert!(validate_key("variant_id").is_ok());
        assert!(validate_key("$feature/senior-triage").is_ok());
        assert!(validate_key("x'; DROP TABLE events; --").is_err());
    }

    #[test]
    fn parses_posthog_experiment_results_payload() {
        let raw = br#"{
            "results": [
                ["triage-v1-mbt", 100, 20, 1, 80],
                ["triage-v3-clinical", 100, 45, 0, 82]
            ]
        }"#;
        let rows = parse_experiment_results(raw).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].variant, "triage-v1-mbt");
        assert!((rows[0].conversion_rate - 0.20).abs() < 1e-9);
        assert!((rows[0].error_rate - 0.01).abs() < 1e-9);
        assert_eq!(rows[1].variant, "triage-v3-clinical");
        assert!((rows[1].conversion_rate - 0.45).abs() < 1e-9);
        assert_eq!(rows[1].errors, 0);
    }
}
