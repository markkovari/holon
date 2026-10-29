//! Google Calendar CLI and daemon (ADR-0125).

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "holon-gcal", about = "Google Calendar provider and OAuth authorization")]
struct Args {
    /// Where client.json and token.json live.
    #[arg(long, env = "HOLON_GOOGLE_DIR")]
    google_dir: Option<PathBuf>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let mut cfg = holon_gcal::GoogleConfig::default();
    if let Some(dir) = args.google_dir {
        cfg.dir = dir;
    }
    eprintln!("holon-gcal: using directory {:?}", cfg.dir);
    Ok(())
}
