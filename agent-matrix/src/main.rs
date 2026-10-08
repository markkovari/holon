//! `agent-matrix`: the Matrix bridge for the agent runtime.
//!
//!   agent-matrix init  --server-name malna.tail3a9c.ts.net --homeserver https://malna.tail3a9c.ts.net \
//!                      --runtime-url http://127.0.0.1:18017 --runtime-token-file <state>/admin-token \
//!                      --appservice-url http://100.81.10.8:9009        # owner password in $HOLON_OWNER_PASSWORD
//!   agent-matrix run   [--config ~/.holon-matrix/bridge.json]

use std::path::PathBuf;

use agent_matrix::init::{init, read_token, InitArgs};
use agent_matrix::{serve::serve, Bridge, Config};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "agent-matrix", about = "Every agent a Matrix user, every project a Space.")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

fn default_dir() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".holon-matrix")
}

#[derive(Subcommand)]
enum Cmd {
    /// Generate tokens, log the owner in once, write the config and the Synapse registration file.
    Init {
        #[arg(long)]
        server_name: String,
        #[arg(long)]
        homeserver: String,
        /// The owner's local part (`mark` for @mark:server). Must be a Synapse admin.
        #[arg(long, default_value = "mark")]
        owner: String,
        /// Read from the environment, never from the command line.
        #[arg(long, env = "HOLON_OWNER_PASSWORD", hide_env_values = true)]
        owner_password: String,
        #[arg(long, default_value = "http://127.0.0.1:18017")]
        runtime_url: String,
        /// File holding the runtime's admin token (`<state-dir>/admin-token`).
        #[arg(long)]
        runtime_token_file: PathBuf,
        #[arg(long, default_value = "0.0.0.0:9009")]
        listen: String,
        /// How SYNAPSE reaches this bridge, e.g. http://<this Mac's tailnet IP>:9009
        #[arg(long)]
        appservice_url: String,
        #[arg(long)]
        dir: Option<PathBuf>,
    },
    /// Run the bridge.
    Run {
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn main() -> Result<(), String> {
    match Cli::parse().cmd {
        Cmd::Init {
            server_name,
            homeserver,
            owner,
            owner_password,
            runtime_url,
            runtime_token_file,
            listen,
            appservice_url,
            dir,
        } => {
            let dir = dir.unwrap_or_else(default_dir);
            let (config, registration) = init(InitArgs {
                server_name,
                homeserver,
                owner_localpart: owner,
                owner_password,
                runtime_url,
                runtime_token: read_token(&runtime_token_file)?,
                listen,
                appservice_url,
                dir,
            })?;
            println!("wrote {}\nwrote {}", config.display(), registration.display());
            println!("Next: give Synapse the registration file (infra/matrix/deploy.sh does it when APPSERVICE_REG is set), restart it, then `agent-matrix run`.");
            Ok(())
        }
        Cmd::Run { config } => {
            let cfg = Config::load(&config.unwrap_or_else(|| default_dir().join("bridge.json")))?;
            let bridge = Bridge::new(cfg);
            bridge.ensure_bridge_user().map_err(|e| format!("registering the bridge user (is the registration file loaded by Synapse?): {e}"))?;
            let addr = serve(bridge.clone())?;
            bridge.spawn_loops();
            eprintln!("agent-matrix: listening for Synapse on http://{addr}");
            loop {
                std::thread::park();
            }
        }
    }
}
