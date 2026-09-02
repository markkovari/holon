//! The inventory bucket's TTL is whatever the bucket says, and a mismatch is noticed.
//!
//! `docs/CURRENT.md` carried this as a gap for as long as it did because it cannot be
//! reasoned about from one process: three of them call `create_key_value` on the same
//! bucket with their own `max_age` — a host asks for `heartbeat_secs * 3`, the
//! reconciler for `--inventory-ttl`, the ingress for its own — and whoever creates it
//! first decides. They agree today only because three defaults coincide at 15s.
//!
//! So this spawns a real `nats-server` and does what no single process can: creates
//! the bucket at one TTL, connects again asking for a different one, and asserts the
//! second connection reports the FIRST one's value rather than its own request.
//!
//! Skips when `nats-server` is not on PATH — a machine without it has broken nothing.

use std::process::{Child, Command, Stdio};
use std::time::Duration;

use comp_lattice::nats::NatsLattice;

/// A port the OS says is free, for the same reason `gatelib` asks rather than hashes:
/// a guessed port collides under a parallel `cargo test` and fails as
/// `Address already in use`, which reads as a bug in the code under test.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS has no free port")
        .local_addr()
        .expect("a bound listener has an address")
        .port()
}

/// A JetStream-enabled `nats-server`, killed when the test ends or panics.
struct Nats {
    child: Child,
    url: String,
    _dir: tempfile::TempDir,
}

impl Drop for Nats {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Nats {
    fn start() -> Option<Self> {
        if Command::new("nats-server").arg("--version").output().is_err() {
            eprintln!("SKIPPED: no nats-server on PATH");
            return None;
        }
        let dir = tempfile::tempdir().expect("tempdir");
        let port = free_port();
        let child = Command::new("nats-server")
            .args(["-js", "-sd"])
            .arg(dir.path().join("nats"))
            .args(["-a", "127.0.0.1", "-p", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn nats-server");
        let url = format!("nats://127.0.0.1:{port}");
        // Poll rather than sleep a flat two seconds: JetStream is ready when a
        // connection succeeds, and waiting for a fixed time is either slow or flaky.
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                std::thread::sleep(Duration::from_millis(300));
                return Some(Self { child, url, _dir: dir });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        panic!("nats-server never accepted a connection on {port}");
    }
}

/// The bucket that already exists wins, and the second process is told so.
#[tokio::test(flavor = "multi_thread")]
async fn the_first_process_to_create_the_bucket_decides_the_ttl() {
    let Some(nats) = Nats::start() else { return };
    let lattice = "ttltest";

    // First process: 15s, the default the three defaults coincide at.
    let first = NatsLattice::connect(&nats.url, lattice, Duration::from_secs(15))
        .await
        .expect("first connect");
    assert_eq!(
        first.effective_ttl(),
        Duration::from_secs(15),
        "the process that CREATED the bucket must get what it asked for"
    );

    // Second process: 60s, as a host with `--heartbeat-secs 20` would ask for. It
    // does not get it, and this is the assertion the gap was about — before
    // `effective_ttl` existed, this process went on to size its refresh against 60.
    //
    // 60 rather than 45 on purpose: 45/3 is exactly 15, and a test whose claim rests
    // on `15 > 15` is testing a boundary instead of the phenomenon. A refresh equal
    // to the whole TTL is already broken, but saying so needs a different assertion
    // than "exceeds", so the numbers are chosen to make the gap unambiguous.
    let second = NatsLattice::connect(&nats.url, lattice, Duration::from_secs(60))
        .await
        .expect("second connect");
    assert_eq!(
        second.effective_ttl(),
        Duration::from_secs(15),
        "the bucket was created at 15s; a later process asking for 60s must be told \
         it has 15s, not handed back its own request"
    );

    // And the arithmetic that actually broke: a third of the ttl.
    //
    // Against the request it is 20s, against reality 5s — so an ingress that trusted
    // its own number would re-read a 15s bucket every 20s and routinely read a bucket
    // that had already expired everything in it. From the outside that is
    // `no app answers`.
    let refresh_from_request = 60 / 3;
    let refresh_from_reality = second.effective_ttl().as_secs() / 3;
    assert_eq!(refresh_from_reality, 5);
    assert!(
        refresh_from_request > second.effective_ttl().as_secs(),
        "the point of this test: sizing a refresh off the REQUEST ({refresh_from_request}s) \
         exceeds the bucket's whole lifetime ({}s)",
        second.effective_ttl().as_secs()
    );
}

/// A fresh bucket gets exactly what it asked for — the case that must keep working,
/// or every deployment reads a warning about nothing.
#[tokio::test(flavor = "multi_thread")]
async fn a_bucket_nobody_created_yet_gets_the_requested_ttl() {
    let Some(nats) = Nats::start() else { return };
    let l = NatsLattice::connect(&nats.url, "freshttl", Duration::from_secs(30))
        .await
        .expect("connect");
    assert_eq!(l.effective_ttl(), Duration::from_secs(30));
}
