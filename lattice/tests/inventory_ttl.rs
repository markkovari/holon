//! Host, reconciler and ingress each create the bucket with their own `max_age`; the first wins.
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use comp_lattice::nats::NatsLattice;

/// OS-assigned: a guessed port collides under a parallel `cargo test`.
fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .expect("the OS has no free port")
        .local_addr()
        .expect("a bound listener has an address")
        .port()
}

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
        let mut child = Command::new("nats-server")
            .args(["-js", "-sd"])
            .arg(dir.path().join("nats"))
            .args(["-a", "127.0.0.1", "-p", &port.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn nats-server");
        let url = format!("nats://127.0.0.1:{port}");
        for _ in 0..100 {
            if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
                std::thread::sleep(Duration::from_millis(300));
                return Some(Self { child, url, _dir: dir });
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        // No `Self`, so no `Drop`: reap it here or it is a zombie.
        let _ = child.kill();
        let _ = child.wait();
        panic!("nats-server never accepted a connection on {port}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_process_to_create_the_bucket_decides_the_ttl() {
    let Some(nats) = Nats::start() else { return };
    let lattice = "ttltest";

    let first = NatsLattice::connect(&nats.url, lattice, Duration::from_secs(15))
        .await
        .expect("first connect");
    assert_eq!(
        first.effective_ttl(),
        Duration::from_secs(15),
        "the process that CREATED the bucket must get what it asked for"
    );

    // 60, not 45: 45/3 is exactly 15, which would test a boundary instead.
    let second = NatsLattice::connect(&nats.url, lattice, Duration::from_secs(60))
        .await
        .expect("second connect");
    assert_eq!(
        second.effective_ttl(),
        Duration::from_secs(15),
        "the bucket was created at 15s; a later process asking for 60s must be told \
         it has 15s, not handed back its own request"
    );

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

#[tokio::test(flavor = "multi_thread")]
async fn a_bucket_nobody_created_yet_gets_the_requested_ttl() {
    let Some(nats) = Nats::start() else { return };
    let l = NatsLattice::connect(&nats.url, "freshttl", Duration::from_secs(30))
        .await
        .expect("connect");
    assert_eq!(l.effective_ttl(), Duration::from_secs(30));
}
