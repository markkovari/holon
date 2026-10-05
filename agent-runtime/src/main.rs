//! `agent-runtime` — the headless daemon: schedules, events and HTTP triggers
//! for every agent in a state directory, with no UI. The console embeds the
//! same library; run this when agents should keep working with it closed.
//!
//!   agent-runtime --state-dir ~/.holon-agents [--listen 127.0.0.1:18017]
//!                 [--local-url http://127.0.0.1:PORT] [--local-model NAME]
//!
//! The admin token is generated on first start and kept in
//! `<state-dir>/admin-token` (mode 0600); control endpoints need it.

use std::path::PathBuf;

use agent_runtime::model::LocalModel;
use agent_runtime::{server, Config, Runtime};

fn main() -> Result<(), String> {
    let mut state = None;
    let mut listen = "127.0.0.1:18017".to_string();
    let mut local = LocalModel::default();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = |what: &str| args.next().ok_or(format!("{what} needs a value"));
        match a.as_str() {
            "--state-dir" => state = Some(PathBuf::from(val("--state-dir")?)),
            "--listen" => listen = val("--listen")?,
            "--local-url" => local.base_url = Some(val("--local-url")?),
            "--local-model" => local.model = val("--local-model")?,
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let state = state.ok_or("--state-dir is required")?;
    std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
    let token = server::admin_token_in(&state)?;

    let mut cfg = Config::new(&state);
    cfg.local = local;
    let rt = Runtime::new(cfg)?;
    let addr = server::serve(rt.clone(), &listen, token)?;
    rt.start_scheduler();
    eprintln!("agent-runtime listening on http://{addr} (state: {})", state.display());
    loop {
        std::thread::park();
    }
}
