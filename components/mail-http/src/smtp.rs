//! One message over SMTP (RFC 5321), for a server on the app's egress such as MailHog: no TLS, no
//! auth, what `comp-mailrelay` spoke for the component before a component could hold a socket.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

use crate::bindings::exports::mail::send::sender::SendError;

/// A CR or LF in an address or subject would end its SMTP line or header and start another.
fn one_line(what: &str, v: &str) -> Result<(), SendError> {
    if v.contains(['\r', '\n']) {
        return Err(SendError::Rejected(format!("{what} has a line break")));
    }
    Ok(())
}

/// The message as DATA carries it, dot-stuffed so a lone "." line cannot end it early.
fn data(from: &str, to: &str, subject: &str, text: &str, id: &str) -> String {
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
    data
}

pub fn send(
    addr: &str,
    from: &str,
    to: &str,
    subject: &str,
    text: &str,
) -> Result<String, SendError> {
    one_line("from", from)?;
    one_line("to", to)?;
    one_line("subject", subject)?;
    if !to.contains('@') {
        return Err(SendError::Rejected(format!("not an address: {to}")));
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let id = format!("{nanos}@holon");
    let down =
        |what: &str, e: &dyn std::fmt::Display| SendError::Unavailable(format!("{what}: {e}"));
    let stream = TcpStream::connect(addr).map_err(|e| down(&format!("connect {addr}"), &e))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10))).ok();
    // One stream both ways: `try_clone` is not supported on wasip2, and `&TcpStream` reads and writes.
    let mut w = &stream;
    let mut r = BufReader::new(&stream);

    let expect = |r: &mut BufReader<&TcpStream>, what: &str| -> Result<(), SendError> {
        let mut line = String::new();
        loop {
            line.clear();
            r.read_line(&mut line).map_err(|e| down(what, &e))?;
            if line.is_empty() {
                return Err(SendError::Unavailable(format!("{what}: connection closed")));
            }
            // A multiline reply has a '-' in the fourth column; the last does not.
            if line.as_bytes().get(3) != Some(&b'-') {
                break;
            }
        }
        match line.as_bytes().first() {
            Some(b'2') | Some(b'3') => Ok(()),
            // 5xx is the server refusing this message; anything else is it not answering.
            Some(b'5') => Err(SendError::Rejected(format!("{what}: {}", line.trim()))),
            _ => Err(SendError::Unavailable(format!("{what}: {}", line.trim()))),
        }
    };

    expect(&mut r, "greeting")?;
    let mut cmd =
        |r: &mut BufReader<&TcpStream>, line: &str, what: &str| -> Result<(), SendError> {
            write!(w, "{line}\r\n").map_err(|e| down(what, &e))?;
            w.flush().ok();
            expect(r, what)
        };
    cmd(&mut r, "EHLO holon", "EHLO")?;
    cmd(&mut r, &format!("MAIL FROM:<{from}>"), "MAIL FROM")?;
    cmd(&mut r, &format!("RCPT TO:<{to}>"), "RCPT TO")?;
    cmd(&mut r, "DATA", "DATA")?;
    w.write_all(data(from, to, subject, text, &id).as_bytes()).map_err(|e| down("body", &e))?;
    w.flush().ok();
    expect(&mut r, "end of DATA")?;
    let _ = write!(w, "QUIT\r\n");
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_break_cannot_smuggle_a_command_or_a_header() {
        let e = send("127.0.0.1:1", "a@b", "x@y\r\nRCPT TO:<z@w>", "s", "t").unwrap_err();
        assert!(matches!(e, SendError::Rejected(s) if s.contains("line break")));
        let e = send("127.0.0.1:1", "a@b", "x@y", "hi\nBcc: z@w", "t").unwrap_err();
        assert!(matches!(e, SendError::Rejected(_)));
    }

    #[test]
    fn a_lone_dot_line_is_stuffed_and_the_message_ends_once() {
        let d = data("a@b", "x@y", "s", "one\n.\n..two", "id");
        assert!(d.ends_with("one\r\n..\r\n...two\r\n.\r\n"), "{d}");
        assert!(d.contains("Message-ID: <id>\r\n"));
    }
}
