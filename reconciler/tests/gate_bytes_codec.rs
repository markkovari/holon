//! The `bytes:codec` specification, run against the composed ARTIFACT.
//!
//! Ported from `components/bytes-codec/gate.sh`. Drives RFC 4648 vectors, alphabet interchangeability,
//! padding rules, malformed refusals, hex encoding, and full 256-byte roundtrip through `codec-probe`.

mod gatelib;
use gatelib::Gate;
use serde_json::Value;

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn norm(s: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        v.to_string()
    } else {
        s.to_string()
    }
}

fn expect_json(gate: &Gate, label: &str, path_and_query: &str, expected_json: &str) {
    let (status, body) = gate.get(path_and_query, None);
    let n_body = norm(&body);
    let n_exp = norm(expected_json);
    assert!(
        n_body == n_exp,
        "FAIL {label} (HTTP {status})\n       expected: {expected_json}\n       got:      {body}"
    );
}

#[test]
fn bytes_codec_passes_against_composed_probe() {
    let Some(gate) = Gate::compose_and_start("codecprobe", "codec-probe", &[]) else {
        return;
    };

    // RFC 4648 vectors
    let vectors = [
        ("f", "Zg=="),
        ("fo", "Zm8="),
        ("foo", "Zm9v"),
        ("foob", "Zm9vYg=="),
        ("fooba", "Zm9vYmE="),
        ("foobar", "Zm9vYmFy"),
    ];
    for (raw, want) in vectors {
        let hex = to_hex(raw.as_bytes());
        expect_json(
            &gate,
            &format!("encode {raw}"),
            &format!("/encode?bytes={hex}&alphabet=standard"),
            &format!(r#"{{"text":"{want}"}}"#),
        );
        let escaped = want.replace('=', "%3D");
        expect_json(
            &gate,
            &format!("decode {want}"),
            &format!("/decode?text={escaped}&alphabet=standard"),
            &format!(r#"{{"bytes":"{hex}"}}"#),
        );
    }

    // Alphabets are not interchangeable
    expect_json(
        &gate,
        "encode fbff standard",
        "/encode?bytes=fbff&alphabet=standard",
        r#"{"text":"+/8="}"#,
    );
    expect_json(
        &gate,
        "encode fbff url-safe",
        "/encode?bytes=fbff&alphabet=url-safe",
        r#"{"text":"-_8"}"#,
    );
    expect_json(
        &gate,
        "standard text refused by url-safe",
        "/decode?text=%2B%2F8%3D&alphabet=url-safe",
        r#"{"at":0,"error":"not-in-alphabet","found":"+"}"#,
    );
    expect_json(
        &gate,
        "url-safe text refused by standard",
        "/decode?text=-_8&alphabet=standard",
        r#"{"at":0,"error":"not-in-alphabet","found":"-"}"#,
    );

    // Padding
    let f_hex = to_hex(b"f");
    expect_json(
        &gate,
        "url-safe does not pad",
        &format!("/encode?bytes={f_hex}&alphabet=url-safe"),
        r#"{"text":"Zg"}"#,
    );
    expect_json(
        &gate,
        "padded url-safe decodes",
        "/decode?text=Zg%3D%3D&alphabet=url-safe",
        &format!(r#"{{"bytes":"{f_hex}"}}"#),
    );
    expect_json(
        &gate,
        "unpadded standard decodes",
        "/decode?text=Zg&alphabet=standard",
        &format!(r#"{{"bytes":"{f_hex}"}}"#),
    );
    expect_json(
        &gate,
        "padding in the middle",
        "/decode?text=Zg%3D%3DZg%3D%3D&alphabet=standard",
        r#"{"at":2,"error":"misplaced-padding"}"#,
    );

    // Refusals
    expect_json(
        &gate,
        "a character in neither",
        "/decode?text=Zm9v%21mFy&alphabet=standard",
        r#"{"at":4,"error":"not-in-alphabet","found":"!"}"#,
    );
    expect_json(
        &gate,
        "an orphan character",
        "/decode?text=Zm9vYmFyZ&alphabet=standard",
        r#"{"error":"truncated-group","length":9}"#,
    );

    // Hex
    expect_json(&gate, "to-hex", "/to-hex?bytes=deadbeef", r#"{"text":"deadbeef"}"#);
    expect_json(
        &gate,
        "from-hex, either case",
        "/from-hex?text=DeAdBeEf",
        r#"{"bytes":"deadbeef"}"#,
    );
    expect_json(
        &gate,
        "from-hex refuses",
        "/from-hex?text=dead%20beef",
        r#"{"at":4,"error":"not-in-alphabet","found":" "}"#,
    );
    expect_json(
        &gate,
        "an odd number of digits",
        "/from-hex?text=abc",
        r#"{"error":"truncated-group","length":3}"#,
    );

    // Every byte survives a round trip
    let all_bytes: Vec<u8> = (0..=255).collect();
    let all_hex = to_hex(&all_bytes);
    for a in ["standard", "url-safe"] {
        let (_, enc_body) = gate.get(&format!("/encode?bytes={all_hex}&alphabet={a}"), None);
        let parsed: Value = serde_json::from_str(&enc_body).unwrap_or(Value::Null);
        let text = parsed["text"].as_str().expect("encoded text");
        let esc = text.replace('+', "%2B").replace('/', "%2F").replace('=', "%3D");
        expect_json(
            &gate,
            &format!("round trip {a}"),
            &format!("/decode?text={esc}&alphabet={a}"),
            &format!(r#"{{"bytes":"{all_hex}"}}"#),
        );
    }
}
