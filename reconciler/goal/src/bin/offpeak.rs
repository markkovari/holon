//! Exits 0 when DeepSeek is off-peak: a cron gate, `comp-offpeak && comp-goalrun ...`.

use clap::Parser;
use holon_fuel::offpeak::{deepseek_off_peak, parse_holidays};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser)]
#[command(name = "comp-offpeak", about = "Is DeepSeek off-peak right now?")]
struct Args {
    /// A file of `YYYY-MM-DD` days (Chinese holidays, adjusted workdays) that are off-peak all day.
    #[arg(long)]
    holidays: Option<PathBuf>,
    /// Check this Unix timestamp (seconds, UTC) instead of now.
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
