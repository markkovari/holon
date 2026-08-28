//! `comp-mailrelay` — HTTP in, real SMTP out. The piece that lets a wasm component
//! reach MailHog.
//!
//! `comp-host` wires no `wasi:sockets`, so a component cannot open a TCP connection
//! and therefore cannot speak SMTP at all. MailHog only ingests SMTP — its HTTP API
//! is read-only. So something outside the sandbox has to bridge the two.
//!
//! It is NOT a mock. It accepts the same JSON body `mail-http` POSTs to Resend and
//! turns it into a genuine SMTP session, which MailHog genuinely receives, stores
//! and serves back. The component under test is byte-identical whether it is pointed
//! here or at Resend, which is the property that makes this worth having and a stub
//! worthless.
//!
//! std only, no dependencies, in the shape of `examples/mesh/src/bin/flaky.rs` — the
//! other small native thing in this repository that exists so a gate has something
//! real to talk to. SMTP's submission path is four commands and a dot; a crate for
//! it would be a dependency for sixty lines.
//!
//!   comp-mailrelay 127.0.0.1:3390 127.0.0.1:1025
//!   POST /  {"from","to":[…],"subject","text"}  ->  201 {"id":"…"}

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::thread;

fn main() {
    let mut args = std::env::args().skip(1);
    let addr = args.next().unwrap_or_else(|| "127.0.0.1:3390".to_string());
    let smtp = args.next().unwrap_or_else(|| "127.0.0.1:1025".to_string());
    let listener = TcpListener::bind(&addr).unwrap_or_else(|e| panic!("bind {addr}: {e}"));
    eprintln!("comp-mailrelay: POST http://{addr}/ -> SMTP {smtp}");
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let smtp = smtp.clone();
                thread::spawn(move || serve(s, &smtp));
            }
            Err(e) => eprintln!("accept: {e}"),
        }
    }
}

fn reply(stream: &mut TcpStream, code: u16, body: &str) {
    let _ = write!(
        stream,
        "HTTP/1.1 {code} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        if code < 300 { "OK" } else { "Error" },
        body.len()
    );
}

fn serve(mut stream: TcpStream, smtp: &str) {
    // Read the head, then exactly `content-length` more. A body cannot be read by
    // "until the socket closes" here: the client keeps it open waiting for us.
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    let head_end = loop {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = find(&buf, b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 1 << 20 {
            return reply(&mut stream, 431, r#"{"error":"header too large"}"#);
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let method = head.split_whitespace().next().unwrap_or("").to_string();

    if method == "GET" {
        // So a gate can tell "the relay is not up" from "the relay refused it".
        return reply(&mut stream, 200, &format!(r#"{{"relay":"mail","smtp":"{smtp}"}}"#));
    }

    let len: usize = head
        .lines()
        .find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case("content-length").then(|| v.trim().parse().ok())?
        })
        .unwrap_or(0);
    let mut body = buf[head_end..].to_vec();
    while body.len() < len {
        let n = match stream.read(&mut chunk) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        body.extend_from_slice(&chunk[..n]);
    }
    let body = String::from_utf8_lossy(&body).to_string();

    let from = json_str(&body, "from").unwrap_or_default();
    // Resend takes `to` as a list; one recipient is still a list of one.
    let to = json_first_of_array(&body, "to").or_else(|| json_str(&body, "to")).unwrap_or_default();
    if from.is_empty() || to.is_empty() {
        return reply(&mut stream, 422, r#"{"error":"from and to are required"}"#);
    }
    let subject = json_str(&body, "subject").unwrap_or_default();
    let text = json_str(&body, "text").or_else(|| json_str(&body, "body")).unwrap_or_default();

    // The id goes ON the message as its Message-ID and is also what the caller gets
    // back, so a gate can find this exact send among everything else in the mailbox.
    let id = format!("{}.{}@relay.local", std::process::id(), now_nanos());
    match send_smtp(smtp, &from, &to, &subject, &text, &id) {
        // 502 rather than 500: the relay is fine and the thing behind it is not,
        // which is what `mail:send`'s `unavailable` means and what a retry might fix.
        Err(e) => reply(&mut stream, 502, &format!(r#"{{"error":"smtp: {}"}}"#, esc(&e))),
        Ok(()) => reply(&mut stream, 201, &format!(r#"{{"id":"{id}"}}"#)),
    }
}

/// SMTP submission: greet, envelope, DATA, dot. Every step's reply must start with
/// a 2 or a 3 — anything else and the message did not arrive, which is the one
/// thing a relay must never report as success.
fn send_smtp(
    addr: &str,
    from: &str,
    to: &str,
    subject: &str,
    text: &str,
    id: &str,
) -> Result<(), String> {
    let stream = TcpStream::connect(addr).map_err(|e| format!("connect {addr}: {e}"))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
    let mut w = stream.try_clone().map_err(|e| e.to_string())?;
    let mut r = BufReader::new(stream);

    let expect = |r: &mut BufReader<TcpStream>, what: &str| -> Result<(), String> {
        let mut line = String::new();
        loop {
            line.clear();
            r.read_line(&mut line).map_err(|e| format!("{what}: {e}"))?;
            if line.is_empty() {
                return Err(format!("{what}: connection closed"));
            }
            // A multiline reply has a '-' in the fourth column; the last does not.
            if line.as_bytes().get(3) != Some(&b'-') {
                break;
            }
        }
        match line.as_bytes().first() {
            Some(b'2') | Some(b'3') => Ok(()),
            _ => Err(format!("{what}: {}", line.trim())),
        }
    };

    expect(&mut r, "greeting")?;
    let mut cmd = |r: &mut BufReader<TcpStream>, line: &str, what: &str| -> Result<(), String> {
        write!(w, "{line}\r\n").map_err(|e| format!("{what}: {e}"))?;
        w.flush().ok();
        expect(r, what)
    };
    cmd(&mut r, "EHLO holon", "EHLO")?;
    cmd(&mut r, &format!("MAIL FROM:<{from}>"), "MAIL FROM")?;
    cmd(&mut r, &format!("RCPT TO:<{to}>"), "RCPT TO")?;
    cmd(&mut r, "DATA", "DATA")?;

    // Dot-stuffing: a line that is exactly "." would end the message early, so a
    // leading dot is doubled. RFC 5321 says so and MailHog will believe us either
    // way, which is why it has to be right here rather than discovered later.
    let mut data = format!(
        "From: {from}\r\nTo: {to}\r\nSubject: {subject}\r\nMessage-ID: <{id}>\r\n\
         Content-Type: text/plain; charset=utf-8\r\n\r\n"
    );
    for line in text.split('\n') {
        let line = line.trim_end_matches('\r');
        if line.starts_with('.') {
            data.push('.');
        }
        data.push_str(line);
        data.push_str("\r\n");
    }
    data.push_str(".\r\n");
    w.write_all(data.as_bytes()).map_err(|e| format!("body: {e}"))?;
    w.flush().ok();
    expect(&mut r, "end of DATA")?;
    let _ = write!(w, "QUIT\r\n");
    Ok(())
}

// ---- the smallest JSON reading that does the job -----------------------------
//
// Three fields out of a body this program also defines the shape of. Pulling
// serde_json in for that would be a dependency for `"to":[" ... "]`.

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// The string value of `"key"`, with escapes undone.
fn json_str(body: &str, key: &str) -> Option<String> {
    let at = body.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = body[at..].trim_start().strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            },
            c => out.push(c),
        }
    }
    None
}

/// The first string in `"key": [ "…" ]`.
fn json_first_of_array(body: &str, key: &str) -> Option<String> {
    let at = body.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = body[at..].trim_start().strip_prefix(':')?.trim_start();
    let inner = rest.strip_prefix('[')?;
    json_str(&format!("\"x\":{}", inner.trim_start()), "x")
        .or_else(|| json_str(&format!("\"x\": {}", inner), "x"))
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', " ")
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}
