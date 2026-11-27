//! `kv-analytics` — local KV-backed provider for `analytics:telemetry@0.1.0`.
//!
//! Stores event counts under deterministic `metrics:collect` keys so
//! `delivery-domain` and other consumers can use `analytics:telemetry` locally
//! offline and swap in `posthog-analytics` via `wac plug` for production.

#[cfg(target_arch = "wasm32")]
#[allow(warnings)]
mod bindings {
    wit_bindgen::generate!({
        path: [
            "../../wit/deps/wasi-io-0.2.0",
            "../../wit/deps/wasi-clocks-0.2.0",
            "../../wit/deps/wasi-keyvalue-0.2.0-draft",
            "../metrics-collect/wit",
            "../../wit/analytics-telemetry",
            "wit",
        ],
        world: "kv:analytics/kv-analytics",
        generate_all,
    });
}

/// Canonical counter key for `(project, experiment_key, variant, event_name)`.
pub fn exp_counter_key(project: &str, exp_key: &str, variant: &str, event: &str) -> String {
    format!("an:{project}:{exp_key}:{variant}:{event}")
}

/// Canonical counter key for plain `(project, event_name)`.
pub fn event_counter_key(project: &str, event: &str) -> String {
    format!("an_ev:{project}:{event}")
}

#[cfg(target_arch = "wasm32")]
mod component {
    use crate::bindings;
    use crate::{event_counter_key, exp_counter_key};

    use bindings::exports::analytics::telemetry::ingest::Guest as IngestGuest;
    use bindings::exports::analytics::telemetry::query::Guest as QueryGuest;
    use bindings::exports::analytics::telemetry::types::{
        AnalyticsError, Event, EventFilter, ExperimentQuery, FunnelStageResult, FunnelStep,
        Interval, Property, SeriesPoint, TimeWindow, VariantSummary,
    };
    use bindings::metrics::collect::collector as metrics;

    struct Component;

    fn prop<'a>(props: &'a [Property], key: &str) -> &'a str {
        props.iter().find(|p| p.key == key).map(|p| p.value.as_str()).unwrap_or("")
    }

    fn map_err(e: metrics::MetricsError) -> AnalyticsError {
        match e {
            metrics::MetricsError::BackendUnavailable(m) => AnalyticsError::BackendUnavailable(m),
        }
    }

    impl IngestGuest for Component {
        fn capture(ev: Event) -> Result<(), AnalyticsError> {
            // Optional idempotency deduplication via `idempotency_key` property.
            let idem = prop(&ev.properties, "idempotency_key");
            if !idem.is_empty() {
                let seen =
                    metrics::incr(&format!("an_idem:{}:{idem}", ev.project), 1).map_err(map_err)?;
                if seen > 1 {
                    return Ok(());
                }
            }

            metrics::incr(&event_counter_key(&ev.project, &ev.name), 1).map_err(map_err)?;

            let exp_key = prop(&ev.properties, "experiment_key");
            let variant = prop(&ev.properties, "variant_id");
            if !exp_key.is_empty() && !variant.is_empty() {
                metrics::incr(&exp_counter_key(&ev.project, exp_key, variant, &ev.name), 1)
                    .map_err(map_err)?;
            }
            Ok(())
        }

        fn capture_batch(events: Vec<Event>) -> Result<u32, AnalyticsError> {
            let mut n = 0u32;
            for ev in events {
                Self::capture(ev)?;
                n += 1;
            }
            Ok(n)
        }

        fn identify(_distinct_id: String, _traits: Vec<Property>) -> Result<(), AnalyticsError> {
            Ok(())
        }
    }

    impl QueryGuest for Component {
        fn count(filter: EventFilter) -> Result<u64, AnalyticsError> {
            metrics::get(&event_counter_key(&filter.project, &filter.event_name)).map_err(map_err)
        }

        fn unique_users(filter: EventFilter) -> Result<u64, AnalyticsError> {
            // shortcut: KV counter backend approximates unique users with event count;
            // plug `posthog-analytics` for exact distinct_id cardinality.
            Self::count(filter)
        }

        fn timeseries(
            _filter: EventFilter,
            _bucket: Interval,
        ) -> Result<Vec<SeriesPoint>, AnalyticsError> {
            Err(AnalyticsError::NotSupported(
                "timeseries requires a time-series provider (e.g. posthog-analytics)".into(),
            ))
        }

        fn funnel(
            project: String,
            steps: Vec<FunnelStep>,
            _window: TimeWindow,
        ) -> Result<Vec<FunnelStageResult>, AnalyticsError> {
            let mut out = Vec::with_capacity(steps.len());
            let mut base = 0u64;
            for (idx, step) in steps.iter().enumerate() {
                let c = metrics::get(&event_counter_key(&project, &step.event_name))
                    .map_err(map_err)?;
                if idx == 0 {
                    base = c;
                }
                let rate = if base == 0 { 0.0 } else { (c as f64 / base as f64).min(1.0) };
                out.push(FunnelStageResult {
                    event_name: step.event_name.clone(),
                    count: c,
                    unique_users: c,
                    conversion_rate: rate,
                });
            }
            Ok(out)
        }

        fn experiment_results(req: ExperimentQuery) -> Result<Vec<VariantSummary>, AnalyticsError> {
            let mut out = Vec::with_capacity(req.variants.len());
            for arm in &req.variants {
                let exposures = metrics::get(&exp_counter_key(
                    &req.project,
                    &req.experiment_key,
                    arm,
                    &req.exposure_event,
                ))
                .map_err(map_err)?;
                let conversions = metrics::get(&exp_counter_key(
                    &req.project,
                    &req.experiment_key,
                    arm,
                    &req.conversion_event,
                ))
                .map_err(map_err)?;
                let errors = if req.error_event.is_empty() {
                    0
                } else {
                    metrics::get(&exp_counter_key(
                        &req.project,
                        &req.experiment_key,
                        arm,
                        &req.error_event,
                    ))
                    .map_err(map_err)?
                };
                let conversion_rate =
                    if exposures == 0 { 0.0 } else { conversions as f64 / exposures as f64 };
                let error_rate =
                    if exposures == 0 { 0.0 } else { errors as f64 / exposures as f64 };
                out.push(VariantSummary {
                    arm: arm.clone(),
                    exposures,
                    conversions,
                    errors,
                    unique_users: exposures,
                    conversion_rate,
                    error_rate,
                });
            }
            Ok(out)
        }

        fn breakdown(
            _filter: EventFilter,
            _property_key: String,
            _limit: u32,
        ) -> Result<Vec<(String, u64)>, AnalyticsError> {
            Err(AnalyticsError::NotSupported(
                "breakdown requires a query provider (e.g. posthog-analytics)".into(),
            ))
        }
    }

    bindings::export!(Component with_types_in bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_keys_are_namespaced_by_project_and_experiment() {
        assert_eq!(
            exp_counter_key("vet-clinic", "deliv-1", "v2", "exposed"),
            "an:vet-clinic:deliv-1:v2:exposed"
        );
        assert_eq!(event_counter_key("vet-clinic", "exposed"), "an_ev:vet-clinic:exposed");
    }
}
