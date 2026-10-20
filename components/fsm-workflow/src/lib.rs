//! `fsm-workflow` — enforce a lifecycle — declare the legal state transitions and refuse the illegal ones

#[allow(warnings)]
mod bindings;

use serde::{Deserialize, Serialize};

use bindings::exports::fsm::workflow::engine::{
    Definition, FsmError, Guest, HistoryEntry, Status, Transition,
};
use bindings::wasi::clocks::wall_clock;
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

fn now() -> u64 {
    wall_clock::now().seconds
}

/// Same byte scheme as idempotency-guard and config-store.
fn sanitize(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn def_key(machine: &str) -> String {
    format!("fsm_def_{}", sanitize(machine))
}

fn inst_key(machine: &str, instance: &str) -> String {
    format!("fsm_inst_{}_{}", sanitize(machine), sanitize(instance))
}

fn hist_key(machine: &str, instance: &str) -> String {
    format!("fsm_hist_{}_{}", sanitize(machine), sanitize(instance))
}

#[derive(Serialize, Deserialize)]
struct StoredDef {
    states: Vec<String>,
    initial: String,
    /// (event, source, target)
    transitions: Vec<(String, String, String)>,
    terminal: Vec<String>,
}

impl StoredDef {
    fn from_wit(def: &Definition) -> Self {
        StoredDef {
            states: def.states.clone(),
            initial: def.initial.clone(),
            transitions: def
                .transitions
                .iter()
                .map(|t| (t.event.clone(), t.source.clone(), t.target.clone()))
                .collect(),
            terminal: def.terminal.clone(),
        }
    }

    fn into_wit(self) -> Definition {
        Definition {
            states: self.states,
            initial: self.initial,
            transitions: self
                .transitions
                .into_iter()
                .map(|(event, source, target)| Transition { event, source, target })
                .collect(),
            terminal: self.terminal,
        }
    }

    fn is_terminal(&self, state: &str) -> bool {
        self.terminal.iter().any(|s| s == state)
    }
}

#[derive(Serialize, Deserialize)]
struct StoredInst {
    state: String,
    steps: u32,
}

#[derive(Serialize, Deserialize)]
struct StoredHist {
    event: String,
    source: String,
    target: String,
    at: u64,
}

impl StoredHist {
    fn into_wit(self) -> HistoryEntry {
        HistoryEntry { event: self.event, source: self.source, target: self.target, at: self.at }
    }
}

fn open() -> Result<kv::Bucket, FsmError> {
    kv::open(BUCKET).map_err(|e| FsmError::BackendUnavailable(format!("open: {e:?}")))
}

fn load_json<T: for<'de> Deserialize<'de>>(
    bucket: &kv::Bucket,
    k: &str,
) -> Result<Option<T>, FsmError> {
    match bucket.get(k) {
        Ok(Some(bytes)) => {
            let v = serde_json::from_slice::<T>(&bytes)
                .map_err(|e| FsmError::BackendUnavailable(format!("decode {k}: {e}")))?;
            Ok(Some(v))
        }
        Ok(None) => Ok(None),
        Err(e) => Err(FsmError::BackendUnavailable(format!("get {k}: {e:?}"))),
    }
}

fn store_json<T: Serialize>(bucket: &kv::Bucket, k: &str, value: &T) -> Result<(), FsmError> {
    let bytes = serde_json::to_vec(value)
        .map_err(|e| FsmError::BackendUnavailable(format!("encode {k}: {e}")))?;
    bucket.set(k, &bytes).map_err(|e| FsmError::BackendUnavailable(format!("set {k}: {e:?}")))
}

fn load_def(bucket: &kv::Bucket, machine: &str) -> Result<StoredDef, FsmError> {
    load_json::<StoredDef>(bucket, &def_key(machine))?.ok_or(FsmError::UnknownMachine)
}

fn load_inst(bucket: &kv::Bucket, machine: &str, instance: &str) -> Result<StoredInst, FsmError> {
    load_json::<StoredInst>(bucket, &inst_key(machine, instance))?.ok_or(FsmError::UnknownInstance)
}

fn load_hist(
    bucket: &kv::Bucket,
    machine: &str,
    instance: &str,
) -> Result<Vec<StoredHist>, FsmError> {
    Ok(load_json::<Vec<StoredHist>>(bucket, &hist_key(machine, instance))?.unwrap_or_default())
}

fn build_status(def: &StoredDef, machine: &str, instance: &str, inst: &StoredInst) -> Status {
    Status {
        machine: machine.to_string(),
        instance: instance.to_string(),
        state: inst.state.clone(),
        done: def.is_terminal(&inst.state),
        steps: inst.steps,
    }
}

impl Guest for Component {
    fn define(name: String, def: Definition) -> Result<(), FsmError> {
        if def.states.is_empty() {
            return Err(FsmError::InvalidDefinition("states must be non-empty".into()));
        }
        let in_states = |s: &str| def.states.iter().any(|x| x == s);

        if !in_states(&def.initial) {
            return Err(FsmError::InvalidDefinition(format!(
                "initial state {:?} is not in states",
                def.initial
            )));
        }
        for t in &def.transitions {
            if !in_states(&t.source) {
                return Err(FsmError::InvalidDefinition(format!(
                    "transition source {:?} (event {:?}) is not in states",
                    t.source, t.event
                )));
            }
            if !in_states(&t.target) {
                return Err(FsmError::InvalidDefinition(format!(
                    "transition target {:?} (event {:?}) is not in states",
                    t.target, t.event
                )));
            }
        }
        for term in &def.terminal {
            if !in_states(term) {
                return Err(FsmError::InvalidDefinition(format!(
                    "terminal state {term:?} is not in states"
                )));
            }
        }

        let bucket = open()?;
        store_json(&bucket, &def_key(&name), &StoredDef::from_wit(&def))
    }

    fn get_definition(name: String) -> Result<Definition, FsmError> {
        let bucket = open()?;
        Ok(load_def(&bucket, &name)?.into_wit())
    }

    fn create_instance(machine: String, instance: String) -> Result<Status, FsmError> {
        let bucket = open()?;
        let def = load_def(&bucket, &machine)?;

        let inst = StoredInst { state: def.initial.clone(), steps: 0 };
        store_json(&bucket, &inst_key(&machine, &instance), &inst)?;
        store_json::<Vec<StoredHist>>(&bucket, &hist_key(&machine, &instance), &Vec::new())?;

        Ok(build_status(&def, &machine, &instance, &inst))
    }

    fn get_status(machine: String, instance: String) -> Result<Status, FsmError> {
        let bucket = open()?;
        let def = load_def(&bucket, &machine)?;
        let inst = load_inst(&bucket, &machine, &instance)?;
        Ok(build_status(&def, &machine, &instance, &inst))
    }

    fn can_fire(machine: String, instance: String, event: String) -> Result<bool, FsmError> {
        let bucket = open()?;
        let def = load_def(&bucket, &machine)?;
        let inst = load_inst(&bucket, &machine, &instance)?;
        Ok(def.transitions.iter().any(|(ev, src, _tgt)| *src == inst.state && *ev == event))
    }

    fn allowed_events(machine: String, instance: String) -> Result<Vec<String>, FsmError> {
        let bucket = open()?;
        let def = load_def(&bucket, &machine)?;
        let inst = load_inst(&bucket, &machine, &instance)?;

        let mut events: Vec<String> = Vec::new();
        for (ev, src, _tgt) in &def.transitions {
            if *src == inst.state && !events.iter().any(|e| e == ev) {
                events.push(ev.clone());
            }
        }
        Ok(events)
    }

    fn fire(machine: String, instance: String, event: String) -> Result<Status, FsmError> {
        let bucket = open()?;
        let def = load_def(&bucket, &machine)?;
        let mut inst = load_inst(&bucket, &machine, &instance)?;

        let target = def
            .transitions
            .iter()
            .find(|(ev, src, _tgt)| *src == inst.state && *ev == event)
            .map(|(_ev, _src, tgt)| tgt.clone())
            .ok_or_else(|| FsmError::IllegalTransition(inst.state.clone()))?;

        let source = inst.state.clone();
        inst.state = target.clone();
        inst.steps = inst.steps.saturating_add(1);

        let mut hist = load_hist(&bucket, &machine, &instance)?;
        hist.push(StoredHist { event: event.clone(), source, target, at: now() });

        store_json(&bucket, &inst_key(&machine, &instance), &inst)?;
        store_json(&bucket, &hist_key(&machine, &instance), &hist)?;

        Ok(build_status(&def, &machine, &instance, &inst))
    }

    fn history(machine: String, instance: String) -> Result<Vec<HistoryEntry>, FsmError> {
        let bucket = open()?;
        let _inst = load_inst(&bucket, &machine, &instance)?;
        let hist = load_hist(&bucket, &machine, &instance)?;
        Ok(hist.into_iter().map(StoredHist::into_wit).collect())
    }
}

bindings::export!(Component with_types_in bindings);
