//! `email-render` — reference implementation of `email:template`.
//!
//! Transactional email template storage + rendering, backed by `wasi:keyvalue`.
//! A template has three fields (subject, text, html), each with `{name}`
//! placeholders. Templates are stored under `tmpl_{name}` as one value: each
//! of the three fields base64-encoded and joined with newlines, so arbitrary
//! field bytes (including newlines) survive a round-trip:
//!   `{b64 subject}\n{b64 text}\n{b64 html}`
//!
//! Rendering is strict: every `{placeholder}` found in any field must have a
//! matching var, otherwise `missing-variable`. For the HTML field only, the
//! substituted variable VALUE is HTML-escaped (`& < > " '`) to prevent HTML/
//! attribute injection from untrusted variable values. The subject and text
//! fields are substituted RAW (not escaped) — they are plain text.

#[allow(warnings)]
mod bindings;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;

use bindings::exports::email::template::renderer::{
    Guest, Message, RenderError, Template, Var,
};
use bindings::wasi::keyvalue::store as kv;

struct Component;

const BUCKET: &str = "default";

// ---- storage ------------------------------------------------------------

/// Sanitize a template name to NATS-legal kv chars (same scheme as the other
/// components' key sanitizers), prefixed with `tmpl_`.
fn tmpl_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 5);
    out.push_str("tmpl_");
    for b in name.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'/' | b'=' => out.push(b as char),
            _ => out.push_str(&format!("_{b:02X}")),
        }
    }
    out
}

fn open() -> Result<kv::Bucket, RenderError> {
    kv::open(BUCKET).map_err(|e| RenderError::BackendUnavailable(format!("open: {e:?}")))
}

/// Serialize a template as `{b64 subject}\n{b64 text}\n{b64 html}`.
fn serialize(tmpl: &Template) -> String {
    format!(
        "{}\n{}\n{}",
        B64.encode(tmpl.subject.as_bytes()),
        B64.encode(tmpl.text.as_bytes()),
        B64.encode(tmpl.html.as_bytes()),
    )
}

/// Parse the stored form back into a `Template`.
fn parse(s: &str) -> Result<Template, RenderError> {
    let mut parts = s.split('\n');
    let subject = decode_field(parts.next())?;
    let text = decode_field(parts.next())?;
    let html = decode_field(parts.next())?;
    Ok(Template {
        subject,
        text,
        html,
    })
}

fn decode_field(part: Option<&str>) -> Result<String, RenderError> {
    let b64 = part.ok_or_else(|| RenderError::BackendUnavailable("corrupt template".into()))?;
    let bytes = B64
        .decode(b64)
        .map_err(|_| RenderError::BackendUnavailable("corrupt template: base64".into()))?;
    String::from_utf8(bytes)
        .map_err(|_| RenderError::BackendUnavailable("corrupt template: utf-8".into()))
}

/// Load a stored template, mapping absence to `unknown-template`.
fn load(bucket: &kv::Bucket, key: &str) -> Result<Template, RenderError> {
    match bucket.get(key) {
        Ok(Some(bytes)) => {
            let s = String::from_utf8(bytes)
                .map_err(|_| RenderError::BackendUnavailable("value not utf-8".into()))?;
            parse(&s)
        }
        Ok(None) => Err(RenderError::UnknownTemplate),
        Err(e) => Err(RenderError::BackendUnavailable(format!("get: {e:?}"))),
    }
}

// ---- rendering ----------------------------------------------------------

fn is_name_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// HTML-escape a value: `& < > " '`. Applied to variable values substituted
/// into the html field to prevent injection.
fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#x27;"),
            _ => out.push(c),
        }
    }
    out
}

/// Look up a var by name.
fn lookup<'a>(vars: &'a [Var], name: &str) -> Option<&'a str> {
    vars.iter()
        .find(|v| v.name == name)
        .map(|v| v.value.as_str())
}

/// Render one field: scan for `{name}` tokens and substitute the matching var.
/// A `{` that does not form a valid token (`{` + name chars + `}`) is left
/// literal. Every valid token requires a var or this returns `missing-variable`.
/// When `escape` is set, the substituted value is HTML-escaped.
fn render_field(field: &str, vars: &[Var], escape: bool) -> Result<String, RenderError> {
    let mut out = String::with_capacity(field.len());
    let bytes = field.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' {
            // try to parse a token: `{` name `}`
            let mut j = i + 1;
            while j < bytes.len() && is_name_char(bytes[j] as char) {
                j += 1;
            }
            if j > i + 1 && j < bytes.len() && bytes[j] == b'}' {
                let name = &field[i + 1..j];
                match lookup(vars, name) {
                    Some(value) => {
                        if escape {
                            out.push_str(&html_escape(value));
                        } else {
                            out.push_str(value);
                        }
                    }
                    None => return Err(RenderError::MissingVariable(name.to_string())),
                }
                i = j + 1;
                continue;
            }
            // not a valid token -> literal `{`
            out.push('{');
            i += 1;
        } else {
            // advance one full utf-8 char to keep the output valid
            let ch_len = utf8_len(bytes[i]);
            out.push_str(&field[i..i + ch_len]);
            i += ch_len;
        }
    }
    Ok(out)
}

/// Byte length of the utf-8 char beginning with `b`.
fn utf8_len(b: u8) -> usize {
    if b < 0x80 {
        1
    } else if b >> 5 == 0b110 {
        2
    } else if b >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// ---- guest --------------------------------------------------------------

impl Guest for Component {
    fn put_template(name: String, tmpl: Template) -> Result<(), RenderError> {
        let bucket = open()?;
        let key = tmpl_key(&name);
        bucket
            .set(&key, serialize(&tmpl).as_bytes())
            .map_err(|e| RenderError::BackendUnavailable(format!("set: {e:?}")))
    }

    fn get_template(name: String) -> Result<Template, RenderError> {
        let bucket = open()?;
        load(&bucket, &tmpl_key(&name))
    }

    fn render(name: String, vars: Vec<Var>) -> Result<Message, RenderError> {
        let bucket = open()?;
        let tmpl = load(&bucket, &tmpl_key(&name))?;
        let subject = render_field(&tmpl.subject, &vars, false)?;
        let text = render_field(&tmpl.text, &vars, false)?;
        let html = render_field(&tmpl.html, &vars, true)?;
        Ok(Message {
            subject,
            text,
            html,
        })
    }
}

bindings::export!(Component with_types_in bindings);
