//! `comp-offpeak` — is DeepSeek off-peak right now? Exits 0 if so, 1 if not.
//!
//! For gating a scheduled `comp-goalrun` invocation (cron / systemd timer) so
//! it only fires during DeepSeek's cheap window, instead of running a
//! long-sleeping foreground process that busy-waits for the clock:
//!
//!     * * * * * comp-offpeak --holidays cn-holidays.txt && comp-goalrun ...
//!
//! The actual off-peak rule lives in `comp_reconciler::offpeak` — this binary
//! is a thin CLI shell around it: read an optional holiday file, read an
//! optional timestamp to check instead of "now", print the verdict, set the
//! exit code.

use clap::Parser;
use comp_reconciler::offpeak::{deepseek_off_peak, parse_holidays};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "comp-offpeak", about = "Is DeepSeek off-peak right now?")]
struct Args {
    /// A file of `YYYY-MM-DD` lines (Chinese public holidays / adjusted
    /// working days) that are off-peak all day regardless of weekday or hour.
    /// Omit for weekday/weekend-only rules with no holiday calendar.
    #[arg(long)]
    holidays: Option<PathBuf>,
    /// Check this Unix timestamp (seconds, UTC) instead of the current time —
    /// for testing the gate against a specific moment.
    #[arg(long)]
    at: Option<u64>,
    /// Print the timestamp and verdict, not just the exit code.
    #[arg(long)]
    verbose: bool,
}

fn main() {
    let args = Args::parse();

    let off_peak_days = match &args.holidays {
        Some(path) => {
            let text = std::fs::read_to_string(path).unwrap_or_else(|e| {
                eprintln!("comp-offpeak: reading {}: {e}", path.display());
                std::process::exit(2);
            });
            parse_holidays(&text).unwrap_or_else(|e| {
                eprintln!("comp-offpeak: {} : {e}", path.display());
                std::process::exit(2);
            })
        }
        None => Vec::new(),
    };

    let now = args.at.unwrap_or_else(|| {
        SystemTime::now().duration_since(UNIX_EPOCH).expect("clock before 1970").as_secs()
    });

    let off_peak = deepseek_off_peak(now, &off_peak_days);

    if args.verbose {
        println!("comp-offpeak: unix_secs={now} off_peak={off_peak}");
    } else {
        println!("{}", if off_peak { "off-peak" } else { "peak" });
    }

    std::process::exit(if off_peak { 0 } else { 1 });
}
