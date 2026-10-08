//! The speech service end to end over HTTP. With stand-in engines (so it runs
//! anywhere ffmpeg does) and, on a Mac with the Swift toolchain, with the real
//! on-device ones. Each skips, saying so, when what it needs is missing.

use std::path::PathBuf;
use std::process::Command;

use agent_runtime::speech::SpeechConfig;
use agent_runtime::{server, Config, Runtime};
use serde_json::Value;

fn have(cmd: &str, arg: &str) -> bool {
    Command::new(cmd).arg(arg).output().is_ok_and(|o| o.status.success())
}

/// A runtime with the given speech engines, served over HTTP; admin token `tok`.
fn serve_with(speech: SpeechConfig) -> (String, tempfile::TempDir) {
    let dir = tempfile::Builder::new().prefix("ar-speech-").tempdir().unwrap();
    let mut cfg = Config::new(dir.path().join("state"));
    cfg.speech = speech;
    let rt = Runtime::new(cfg).unwrap();
    let addr = server::serve(rt, "127.0.0.1:0", "tok".into()).unwrap();
    (format!("http://{addr}"), dir)
}

fn script(dir: &std::path::Path, name: &str, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn speech_is_off_until_engines_are_configured_and_needs_the_admin_token() {
    let (base, _d) = serve_with(SpeechConfig::default());
    let c = reqwest::blocking::Client::new();
    assert_eq!(c.get(format!("{base}/speech")).send().unwrap().status(), 401);
    let caps: Value =
        c.get(format!("{base}/speech")).bearer_auth("tok").send().unwrap().json().unwrap();
    assert_eq!(caps["transcribe"], false, "no STT command configured");
    let r = c
        .post(format!("{base}/speech/transcribe"))
        .bearer_auth("tok")
        .body(vec![1, 2, 3])
        .send()
        .unwrap();
    assert_eq!(r.status(), 501);
    assert!(r.text().unwrap().contains("not configured"));
    assert_eq!(c.post(format!("{base}/speech/speak")).body("hi").send().unwrap().status(), 401);
}

#[test]
fn standin_engines_round_trip_through_the_http_api() {
    if !have("ffmpeg", "-version") {
        eprintln!("skipping: ffmpeg is not installed");
        return;
    }
    let tools = tempfile::Builder::new().prefix("ar-speech-tools-").tempdir().unwrap();
    // STT stand-in: reports how many bytes of decoded audio it got and the language it was asked for.
    let stt = script(tools.path(), "stt", r#"echo "heard $(wc -c < "$1") bytes in $2""#);
    // TTS stand-in: ignores the text, writes one second of a tone to the file it is given.
    let tts = script(
        tools.path(),
        "tts",
        r#"cat >/dev/null; ffmpeg -loglevel error -y -f lavfi -i sine=frequency=440:duration=1 -ar 22050 "$1""#,
    );
    let (base, _d) =
        serve_with(SpeechConfig { stt: Some(stt), tts: Some(tts), ..Default::default() });
    let c = reqwest::blocking::Client::new();

    let caps: Value =
        c.get(format!("{base}/speech")).bearer_auth("tok").send().unwrap().json().unwrap();
    assert_eq!((caps["transcribe"].as_bool(), caps["speak"].as_bool()), (Some(true), Some(true)));

    let r = c
        .post(format!("{base}/speech/speak"))
        .bearer_auth("tok")
        .body("Keep the pace steady.")
        .send()
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers()["content-type"], "audio/ogg");
    let ms: u64 = r.headers()["x-holon-duration-ms"].to_str().unwrap().parse().unwrap();
    assert!((900..=1200).contains(&ms), "about a second of audio, got {ms} ms");
    let wave: Vec<u16> = r.headers()["x-holon-waveform"]
        .to_str()
        .unwrap()
        .split(',')
        .map(|v| v.parse().unwrap())
        .collect();
    assert_eq!(wave.len(), 100);
    assert!(
        wave.iter().all(|v| *v <= 1024) && wave.iter().any(|v| *v > 900),
        "a steady tone has a loud, flat waveform"
    );
    let audio = r.bytes().unwrap();
    assert_eq!(&audio[..4], b"OggS", "an Ogg container, i.e. a voice message");

    // the same audio back through recognition: it is decoded to 16 kHz mono WAV first
    let t: Value = c
        .post(format!("{base}/speech/transcribe?lang=de-DE"))
        .bearer_auth("tok")
        .body(audio.to_vec())
        .send()
        .unwrap()
        .json()
        .unwrap();
    let text = t["text"].as_str().unwrap();
    assert!(text.ends_with("in de-DE"), "{text}");
    let bytes: u64 = text.split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(
        (30_000..=40_000).contains(&bytes),
        "about 1 s of 16 kHz 16-bit mono WAV, got {bytes} bytes"
    );

    // bad input is a 422, not a crash or a command line
    let bad_lang = c
        .post(format!("{base}/speech/transcribe?lang=--bad"))
        .bearer_auth("tok")
        .body(audio.to_vec())
        .send()
        .unwrap();
    assert_eq!(bad_lang.status(), 422);
    let not_audio = c
        .post(format!("{base}/speech/transcribe"))
        .bearer_auth("tok")
        .body("this is not audio")
        .send()
        .unwrap();
    assert_eq!(not_audio.status(), 422);
    assert!(not_audio.text().unwrap().contains("could not decode"));
    let bad_voice = c
        .post(format!("{base}/speech/speak?voice=--help"))
        .bearer_auth("tok")
        .body("hi")
        .send()
        .unwrap();
    assert_eq!(bad_voice.status(), 422);
    assert_eq!(
        c.post(format!("{base}/speech/speak"))
            .bearer_auth("tok")
            .body("   ")
            .send()
            .unwrap()
            .status(),
        422
    );
}

#[test]
fn the_real_on_device_engines_hear_what_the_real_voice_says() {
    if !(cfg!(target_os = "macos")
        && have("say", "-v?")
        && have("ffmpeg", "-version")
        && have("swiftc", "--version"))
    {
        eprintln!("skipping: needs macOS with say, ffmpeg and the Swift toolchain");
        return;
    }
    let tools = tempfile::Builder::new().prefix("ar-speech-real-").tempdir().unwrap();
    let stt = tools.path().join("holon-stt");
    let build = Command::new("swiftc")
        .args(["-parse-as-library", "-O", "-o"])
        .arg(&stt)
        .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("speech/holon-stt.swift"))
        .output()
        .unwrap();
    if !build.status.success() {
        eprintln!(
            "skipping: holon-stt did not build (needs the macOS 26 SDK): {}",
            String::from_utf8_lossy(&build.stderr)
        );
        return;
    }
    let (base, _d) = serve_with(SpeechConfig { stt: Some(stt), ..Default::default() });
    let c = reqwest::blocking::Client::new();

    let said =
        "Alright, last workout, one hour, eleven thousand nine hundred and sixty eight metres.";
    let audio = c
        .post(format!("{base}/speech/speak"))
        .bearer_auth("tok")
        .body(said)
        .send()
        .unwrap()
        .bytes()
        .unwrap();
    assert_eq!(&audio[..4], b"OggS");
    let t: Value = c
        .post(format!("{base}/speech/transcribe"))
        .bearer_auth("tok")
        .body(audio.to_vec())
        .send()
        .unwrap()
        .json()
        .unwrap();
    let heard = t["text"].as_str().unwrap().to_lowercase();
    assert!(heard.contains("last workout") && heard.contains("hour"), "heard: {heard}");
    assert!(
        heard.contains("11,968") || heard.contains("11968") || heard.contains("eleven thousand"),
        "heard: {heard}"
    );

    // a language Apple has no model for is a clear refusal, not garbage
    let hu = c
        .post(format!("{base}/speech/transcribe?lang=hu-HU"))
        .bearer_auth("tok")
        .body(audio.to_vec())
        .send()
        .unwrap();
    assert_eq!(hu.status(), 422);
    assert!(
        hu.text().unwrap().contains("no on-device speech model"),
        "the helper's reason reaches the caller"
    );
}
