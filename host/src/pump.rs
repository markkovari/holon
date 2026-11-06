//! The host's own clock for an app's pull contracts (ADR-0130): `POST <path>` to itself every
//! `every`, what `comp-relay` did from outside. The target is the app this host serves, so there is
//! no URL to configure, and so nothing else it could be pointed at.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::time::Duration;

/// This host's own listener, on loopback when it listens on every address.
pub fn target(addr: SocketAddr, path: &str) -> String {
    let ip = match addr.ip() {
        IpAddr::V4(v) if v.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v) if v.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    format!("http://{}{path}", SocketAddr::new(ip, addr.port()))
}

/// The first pump goes at once: it is also the catch-up after the host was down.
pub fn spawn(url: String, every: Duration) {
    tokio::spawn(async move {
        let http =
            match reqwest::Client::builder().timeout(every.max(Duration::from_secs(30))).build() {
                Ok(c) => c,
                Err(e) => return eprintln!("comp-host: no pump: {e}"),
            };
        let mut tick = tokio::time::interval(every);
        // A pump still running when the next is due is skipped, not queued behind it.
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            match http.post(&url).send().await {
                Ok(r) if r.status().is_success() => {}
                Ok(r) => eprintln!("comp-host: pump {url} answered {}", r.status()),
                Err(e) => eprintln!("comp-host: pump {url}: {}", e.without_url()),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::target;

    #[test]
    fn the_pump_dials_this_host_on_loopback() {
        assert_eq!(
            target("0.0.0.0:3012".parse().unwrap(), "/internal/pump"),
            "http://127.0.0.1:3012/internal/pump"
        );
        assert_eq!(target("[::]:80".parse().unwrap(), "/p"), "http://[::1]:80/p");
        assert_eq!(target("10.0.0.2:3401".parse().unwrap(), "/p"), "http://10.0.0.2:3401/p");
    }
}
