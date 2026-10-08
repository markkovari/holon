//! Local model servers that run only when needed.
//!
//! A big local model holds gigabytes of memory whether or not anyone is using it. An OpenAi
//! model spec may name a launchd `service` that serves it: the runtime starts that service the
//! first time a call needs it (`launchctl kickstart`), waits for the server to answer, and stops
//! it again after it has been idle for a while. Hosted models (DeepSeek) are tried first in a
//! fallback chain, so in the normal case these never start.

use std::collections::HashMap;
use std::process::Command;
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

/// How long a started server may sit unused before it is stopped.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// How long to wait for a just-started server to load its model.
const START_WAIT: Duration = Duration::from_secs(240);

fn last_use() -> &'static Mutex<HashMap<String, Instant>> {
    static M: OnceLock<Mutex<HashMap<String, Instant>>> = OnceLock::new();
    M.get_or_init(Default::default)
}

fn valid(service: &str) -> bool {
    !service.is_empty()
        && service.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

fn target(service: &str) -> Option<String> {
    let uid = Command::new("/usr/bin/id").arg("-u").output().ok()?;
    Some(format!("gui/{}/{service}", String::from_utf8_lossy(&uid.stdout).trim()))
}

fn up(base: &str) -> bool {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()
        .and_then(|c| c.get(format!("{}/v1/models", base.trim_end_matches('/'))).send().ok())
        .is_some_and(|r| r.status().is_success())
}

/// Makes sure the server for `service` answers at `base`, starting it if it does not.
pub fn ensure(service: &str, base: &str) -> Result<(), String> {
    if !valid(service) {
        return Err(format!("bad service name `{service}`"));
    }
    reaper();
    touch(service);
    if up(base) {
        return Ok(());
    }
    let t = target(service).ok_or("could not work out the launchd domain")?;
    let started = Command::new("/bin/launchctl").args(["kickstart", &t]).output();
    match started {
        Ok(o) if o.status.success() => {}
        Ok(o) => {
            return Err(format!(
                "could not start {service}: {}",
                String::from_utf8_lossy(&o.stderr).trim()
            ))
        }
        Err(e) => return Err(format!("could not start {service}: {e}")),
    }
    let deadline = Instant::now() + START_WAIT;
    while Instant::now() < deadline {
        if up(base) {
            touch(service);
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(2));
    }
    Err(format!("{service} did not come up within {}s", START_WAIT.as_secs()))
}

pub fn touch(service: &str) {
    last_use().lock().unwrap().insert(service.to_string(), Instant::now());
}

/// One background thread that stops services idle longer than `IDLE`.
fn reaper() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(|| loop {
            std::thread::sleep(Duration::from_secs(30));
            let idle: Vec<String> = {
                let mut m = last_use().lock().unwrap();
                let gone: Vec<String> =
                    m.iter().filter(|(_, t)| t.elapsed() > IDLE).map(|(s, _)| s.clone()).collect();
                for s in &gone {
                    m.remove(s);
                }
                gone
            };
            for s in idle {
                if let Some(t) = target(&s) {
                    let _ = Command::new("/bin/launchctl").args(["kill", "SIGTERM", &t]).output();
                }
            }
        });
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_labels_are_services_and_a_dead_server_is_not_up() {
        assert!(valid("io.holon.qwen-large"));
        assert!(!valid(""));
        assert!(!valid("io.holon.x; rm -rf /"));
        assert!(!valid("../x"));
        assert!(!up("http://127.0.0.1:1"));
        assert!(ensure("bad service", "http://127.0.0.1:1").is_err());
    }
}
