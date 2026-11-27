//! `posthog-analytics` — self-hosted PostHog provider for `analytics:telemetry@0.1.0`.
//!
//! Talks to any self-hosted PostHog instance over `wasi:http/outgoing-handler`:
//! - Read queries (`count`, `unique-users`, `timeseries`, `funnel`,
//!   `experiment-results`, `breakdown`) execute HogQL via
//!   `POST /api/projects/{project_id}/query/`.
//! - Write calls (`capture`, `capture-batch`, `identify`) post to
//!   `/capture/` and `/batch/`.

pub mod posthog;

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-random-0.2.0",
            "../../wit/deps/wasi-cli-0.2.0",
            "../../wit/deps/wasi-http-0.2.0",
            "../../wit/deps/wasi-config-0.2.0-rc.1",
            "../../host/wit/deps/comp-secrets",
            "../../wit/analytics-telemetry",
            "wit",
        ],
        world: "posthog:analytics/posthog-analytics",
        generate_all,
    });
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::bindings;
    use crate::posthog::{self, Bucket, CaptureEvent, ExpQuery, Filter, Prop, Window};

    use bindings::comp::secrets::reader as secrets;
    use bindings::exports::analytics::telemetry::ingest::Guest as IngestGuest;
    use bindings::exports::analytics::telemetry::query::Guest as QueryGuest;
    use bindings::exports::analytics::telemetry::types::{
        AnalyticsError, Event, EventFilter, ExperimentQuery, FunnelStageResult, FunnelStep,
        Interval, Property, SeriesPoint, TimeWindow, VariantSummary,
    };
    use bindings::wasi::config::store as config;
    use bindings::wasi::http::types::{
        Fields, Method, OutgoingBody, OutgoingRequest, RequestOptions, Scheme,
    };
    use bindings::wasi::io::streams::{OutputStream, StreamError};

    struct Component;

    struct Endpoint {
        scheme: Scheme,
        authority: String,
        prefix: String,
        project_id: String,
    }

    fn cfg(key: &str, default: &str) -> String {
        config::get(key)
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| default.to_string())
    }

    fn secret(key: &str) -> String {
        match secrets::get(key) {
            Ok(Some(handle)) => secrets::reveal(&handle).unwrap_or_default(),
            _ => String::new(),
        }
    }

    impl Endpoint {
        fn open() -> Result<Self, AnalyticsError> {
            let url = cfg("posthog-host", "http://127.0.0.1:8000");
            let (scheme, rest) = match url.split_once("://") {
                Some(("https", r)) => (Scheme::Https, r),
                Some(("http", r)) => (Scheme::Http, r),
                _ => {
                    return Err(AnalyticsError::InvalidQuery(format!(
                        "posthog-host must start with http:// or https://, got {url:?}"
                    )))
                }
            };
            let (authority, prefix) = match rest.split_once('/') {
                Some((a, p)) => (a.to_string(), format!("/{}", p.trim_end_matches('/'))),
                None => (rest.to_string(), String::new()),
            };
            let project_id = cfg("posthog-project-id", "1");
            Ok(Self { scheme, authority, prefix, project_id })
        }

        fn path(&self, tail: &str) -> String {
            format!("{}{tail}", self.prefix)
        }
    }

    fn write_all(stream: &OutputStream, mut data: &[u8]) -> Result<(), AnalyticsError> {
        while !data.is_empty() {
            stream.subscribe().block();
            let permit = stream
                .check_write()
                .map_err(|e| AnalyticsError::BackendUnavailable(format!("check_write: {e:?}")))?;
            if permit == 0 {
                continue;
            }
            let n = (permit as usize).min(data.len());
            stream
                .write(&data[..n])
                .map_err(|e| AnalyticsError::BackendUnavailable(format!("write: {e:?}")))?;
            data = &data[n..];
        }
        stream
            .blocking_flush()
            .map_err(|e| AnalyticsError::BackendUnavailable(format!("flush: {e:?}")))
    }

    fn post_json(
        ep: &Endpoint,
        path: &str,
        bearer: Option<&str>,
        body: &str,
    ) -> Result<Vec<u8>, AnalyticsError> {
        let headers = Fields::new();
        let _ = headers.set("content-type", &[b"application/json".to_vec()]);
        let _ = headers.set("accept", &[b"application/json".to_vec()]);
        if let Some(tok) = bearer {
            let _ = headers.set("authorization", &[format!("Bearer {tok}").into_bytes()]);
        }

        let req = OutgoingRequest::new(headers);
        let net = |m: &str| AnalyticsError::BackendUnavailable(m.to_string());
        req.set_method(&Method::Post).map_err(|_| net("set method"))?;
        req.set_scheme(Some(&ep.scheme)).map_err(|_| net("set scheme"))?;
        req.set_authority(Some(&ep.authority)).map_err(|_| net("set authority"))?;
        req.set_path_with_query(Some(path)).map_err(|_| net("set path"))?;

        let out = req.body().map_err(|_| net("request body"))?;
        {
            let stream = out.write().map_err(|_| net("request stream"))?;
            write_all(&stream, body.as_bytes())?;
        }
        OutgoingBody::finish(out, None).map_err(|_| net("finish body"))?;

        let opts = RequestOptions::new();
        let _ = opts.set_connect_timeout(Some(10_000_000_000));
        let _ = opts.set_first_byte_timeout(Some(30_000_000_000));

        let fut = bindings::wasi::http::outgoing_handler::handle(req, Some(opts))
            .map_err(|e| AnalyticsError::BackendUnavailable(format!("send: {e:?}")))?;
        fut.subscribe().block();
        let resp = fut
            .get()
            .ok_or_else(|| net("no response"))?
            .map_err(|_| net("response taken"))?
            .map_err(|e| AnalyticsError::BackendUnavailable(format!("connect: {e:?}")))?;

        let status = resp.status();
        let incoming = resp.consume().map_err(|_| net("consume body"))?;
        let stream = incoming.stream().map_err(|_| net("response stream"))?;
        let mut buf = Vec::new();
        loop {
            match stream.blocking_read(64 * 1024) {
                Ok(chunk) if chunk.is_empty() => break,
                Ok(chunk) => buf.extend_from_slice(&chunk),
                Err(StreamError::Closed) => break,
                Err(e) => return Err(AnalyticsError::BackendUnavailable(format!("read: {e:?}"))),
            }
        }
        drop(stream);
        drop(incoming);

        match status {
            200..=299 => Ok(buf),
            401 | 403 => Err(AnalyticsError::Unauthorized),
            429 => Err(AnalyticsError::RateLimited),
            400 | 422 => {
                Err(AnalyticsError::InvalidQuery(String::from_utf8_lossy(&buf).into_owned()))
            }
            _ => Err(AnalyticsError::BackendUnavailable(format!(
                "posthog HTTP {status}: {}",
                String::from_utf8_lossy(&buf)
            ))),
        }
    }

    fn run_hogql(hogql: &str) -> Result<Vec<u8>, AnalyticsError> {
        let ep = Endpoint::open()?;
        let api_key = secret("posthog-api-key");
        if api_key.is_empty() {
            return Err(AnalyticsError::Unauthorized);
        }
        let path = ep.path(&format!("/api/projects/{}/query/", ep.project_id));
        let body = posthog::query_body(hogql);
        post_json(&ep, &path, Some(&api_key), &body)
    }

    fn conv_props(props: &[Property]) -> Vec<Prop> {
        props.iter().map(|p| Prop { key: p.key.clone(), value: p.value.clone() }).collect()
    }

    fn conv_window(w: TimeWindow) -> Window {
        Window { from_unix: w.from_unix, to_unix: w.to_unix }
    }

    fn conv_filter(f: &EventFilter) -> Filter {
        Filter {
            event_name: f.event_name.clone(),
            project: f.project.clone(),
            property_filters: conv_props(&f.property_filters),
            window: conv_window(f.window),
        }
    }

    fn conv_event(e: &Event) -> CaptureEvent {
        CaptureEvent {
            name: e.name.clone(),
            distinct_id: e.distinct_id.clone(),
            project: e.project.clone(),
            timestamp_unix: e.timestamp_unix,
            properties: conv_props(&e.properties),
        }
    }

    impl IngestGuest for Component {
        fn capture(ev: Event) -> Result<(), AnalyticsError> {
            let ep = Endpoint::open()?;
            let token = secret("posthog-project-token");
            if token.is_empty() {
                return Err(AnalyticsError::Unauthorized);
            }
            let body = posthog::capture_body(&token, &conv_event(&ev));
            let _ = post_json(&ep, &ep.path("/capture/"), None, &body)?;
            Ok(())
        }

        fn capture_batch(events: Vec<Event>) -> Result<u32, AnalyticsError> {
            if events.is_empty() {
                return Ok(0);
            }
            let ep = Endpoint::open()?;
            let token = secret("posthog-project-token");
            if token.is_empty() {
                return Err(AnalyticsError::Unauthorized);
            }
            let converted: Vec<CaptureEvent> = events.iter().map(conv_event).collect();
            let body = posthog::batch_body(&token, &converted);
            let _ = post_json(&ep, &ep.path("/batch/"), None, &body)?;
            Ok(converted.len() as u32)
        }

        fn identify(distinct_id: String, traits: Vec<Property>) -> Result<(), AnalyticsError> {
            let ep = Endpoint::open()?;
            let token = secret("posthog-project-token");
            if token.is_empty() {
                return Err(AnalyticsError::Unauthorized);
            }
            let body = posthog::identify_body(&token, &distinct_id, &conv_props(&traits));
            let _ = post_json(&ep, &ep.path("/capture/"), None, &body)?;
            Ok(())
        }
    }

    impl QueryGuest for Component {
        fn count(filter: EventFilter) -> Result<u64, AnalyticsError> {
            let q = posthog::hogql_count(&conv_filter(&filter))
                .map_err(AnalyticsError::InvalidQuery)?;
            let raw = run_hogql(&q)?;
            posthog::parse_single_u64(&raw).map_err(AnalyticsError::BackendUnavailable)
        }

        fn unique_users(filter: EventFilter) -> Result<u64, AnalyticsError> {
            let q = posthog::hogql_unique_users(&conv_filter(&filter))
                .map_err(AnalyticsError::InvalidQuery)?;
            let raw = run_hogql(&q)?;
            posthog::parse_single_u64(&raw).map_err(AnalyticsError::BackendUnavailable)
        }

        fn timeseries(
            filter: EventFilter,
            bucket: Interval,
        ) -> Result<Vec<SeriesPoint>, AnalyticsError> {
            let b = match bucket {
                Interval::Hour => Bucket::Hour,
                Interval::Day => Bucket::Day,
                Interval::Week => Bucket::Week,
            };
            let q = posthog::hogql_timeseries(&conv_filter(&filter), b)
                .map_err(AnalyticsError::InvalidQuery)?;
            let raw = run_hogql(&q)?;
            let rows =
                posthog::parse_timeseries(&raw).map_err(AnalyticsError::BackendUnavailable)?;
            Ok(rows
                .into_iter()
                .map(|r| SeriesPoint {
                    bucket_unix: r.bucket_unix,
                    count: r.count,
                    unique_users: r.unique_users,
                })
                .collect())
        }

        fn funnel(
            project: String,
            steps: Vec<FunnelStep>,
            window: TimeWindow,
        ) -> Result<Vec<FunnelStageResult>, AnalyticsError> {
            if steps.is_empty() {
                return Ok(Vec::new());
            }
            let mut results = Vec::with_capacity(steps.len());
            let mut base_users: u64 = 0;
            for (idx, step) in steps.iter().enumerate() {
                let f = Filter {
                    event_name: step.event_name.clone(),
                    project: project.clone(),
                    property_filters: conv_props(&step.property_filters),
                    window: conv_window(window),
                };
                let count_q = posthog::hogql_count(&f).map_err(AnalyticsError::InvalidQuery)?;
                let uniq_q =
                    posthog::hogql_unique_users(&f).map_err(AnalyticsError::InvalidQuery)?;
                let c = posthog::parse_single_u64(&run_hogql(&count_q)?)
                    .map_err(AnalyticsError::BackendUnavailable)?;
                let u = posthog::parse_single_u64(&run_hogql(&uniq_q)?)
                    .map_err(AnalyticsError::BackendUnavailable)?;
                if idx == 0 {
                    base_users = u;
                }
                let conversion_rate =
                    if base_users == 0 { 0.0 } else { (u as f64 / base_users as f64).min(1.0) };
                results.push(FunnelStageResult {
                    event_name: step.event_name.clone(),
                    count: c,
                    unique_users: u,
                    conversion_rate,
                });
            }
            Ok(results)
        }

        fn experiment_results(req: ExperimentQuery) -> Result<Vec<VariantSummary>, AnalyticsError> {
            let q = posthog::hogql_experiment_results(&ExpQuery {
                project: req.project,
                experiment_key: req.experiment_key,
                exposure_event: req.exposure_event,
                conversion_event: req.conversion_event,
                error_event: req.error_event,
                variant_property: req.variant_property,
                variants: req.variants,
                window: conv_window(req.window),
            })
            .map_err(AnalyticsError::InvalidQuery)?;
            let raw = run_hogql(&q)?;
            let rows = posthog::parse_experiment_results(&raw)
                .map_err(AnalyticsError::BackendUnavailable)?;
            Ok(rows
                .into_iter()
                .map(|r| VariantSummary {
                    arm: r.variant,
                    exposures: r.exposures,
                    conversions: r.conversions,
                    errors: r.errors,
                    unique_users: r.unique_users,
                    conversion_rate: r.conversion_rate,
                    error_rate: r.error_rate,
                })
                .collect())
        }

        fn breakdown(
            filter: EventFilter,
            property_key: String,
            limit: u32,
        ) -> Result<Vec<(String, u64)>, AnalyticsError> {
            let q = posthog::hogql_breakdown(&conv_filter(&filter), &property_key, limit)
                .map_err(AnalyticsError::InvalidQuery)?;
            let raw = run_hogql(&q)?;
            posthog::parse_breakdown(&raw).map_err(AnalyticsError::BackendUnavailable)
        }
    }

    bindings::export!(Component with_types_in bindings);
}
