//! `event-pusher` — push delivery for the pull-based event bus — drives consumer polls on hosts with a messaging plugin
//! `$KV.<bucket>.eb.seq.<topic>` change notification; each one POSTs the
//! configured drain paths through proxy:route. Consumers' drains are
//! idempotent and cheap when empty, so no debounce.
//! ponytail: fan out to every target on any topic; add a topic->target map
//! only if drain cost ever matters.

#[allow(warnings)]
mod bindings;

use bindings::exports::wasmcloud::messaging::handler::Guest;
use bindings::proxy::route::router;
use bindings::wasi::config::store as config;
use bindings::wasmcloud::messaging::types::BrokerMessage;

struct Component;

impl Guest for Component {
    fn handle_message(msg: BrokerMessage) -> Result<(), String> {
        // Defense in depth: the subscription should already be scoped to
        // eb.seq.>, but never react to (or loop on) other KV traffic.
        if !msg.subject.contains(".eb.seq.") {
            return Ok(());
        }
        let targets = config::get("push-targets")
            .ok()
            .flatten()
            .unwrap_or_default();
        for path in targets.split(',').map(str::trim).filter(|p| !p.is_empty()) {
            // Best-effort poke: a missed one is caught by the sweep timer.
            let _ = router::forward("POST", path, &[], &[]);
        }
        Ok(())
    }
}

bindings::export!(Component with_types_in bindings);
