//! Speech: text to audio and audio to text, on-device. One implementation that the
//! agents' `speak` / `transcribe` tools, the HTTP API (`/speech/...`) and the Matrix
//! bridge all use.
//!
//! The engines are commands, so they can be swapped without touching this code:
//!
//! * **STT** — `<stt-bin> <audio.wav> <locale>` prints the transcript on stdout. The
//!   default is `speech/holon-stt.swift`, Apple's on-device recognizer (macOS 26+,
//!   headless, no permission prompt, no network). Anything with the same contract
//!   works: a whisper.cpp wrapper, for a language Apple has no model for (Hungarian).
//! * **TTS** — by default the system `say`. An override has the contract
//!   `<tts-bin> <out-audio-file> <voice>` with the text on stdin; it may write any
//!   format ffmpeg reads.
//! * **ffmpeg** is the glue: whatever audio comes in is converted to 16 kHz mono
//!   WAV for recognition, and speech goes out as Opus in Ogg, which is what a Matrix
//!   (and most chat) voice message is.
//!
//! Text goes to `say` on stdin and file names are ours, so nothing the user or a
//! model says can become a command-line option.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const MAX_AUDIO_IN: usize = 25 * 1024 * 1024;
pub const MAX_TEXT: usize = 5_000;
const WAVEFORM_BINS: usize = 100;
const TOOL_TIMEOUT: Duration = Duration::from_secs(120);

#[derive(Clone, Debug)]
pub struct SpeechConfig {
    /// Speech-to-text command; `None` means transcription is unavailable.
    pub stt: Option<PathBuf>,
    /// Text-to-speech override; `None` means the system `say` (macOS).
    pub tts: Option<PathBuf>,
    pub ffmpeg: String,
    /// Recognition language when the caller does not say.
    pub default_locale: String,
}

impl Default for SpeechConfig {
    fn default() -> Self {
        Self { stt: None, tts: None, ffmpeg: "ffmpeg".into(), default_locale: "en-US".into() }
    }
}

/// Spoken audio, with what a chat client needs to show it.
#[derive(Debug, Clone)]
pub struct Spoken {
    /// Opus in Ogg.
    pub audio: Vec<u8>,
    pub duration_ms: u64,
    /// 100 loudness samples, 0..=1024 (what a voice-message waveform wants).
    pub waveform: Vec<u16>,
}

#[derive(Clone)]
pub struct Speech {
    cfg: SpeechConfig,
}

fn works(cmd: &str, arg: &str) -> bool {
    Command::new(cmd)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Runs a command to completion or kills it after `TOOL_TIMEOUT`.
fn run(mut cmd: Command, stdin: Option<&[u8]>) -> Result<(Vec<u8>, Vec<u8>), String> {
    cmd.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let name = format!("{:?}", cmd.get_program());
    let mut child = cmd.spawn().map_err(|e| format!("could not run {name}: {e}"))?;
    if let (Some(data), Some(mut si)) = (stdin, child.stdin.take()) {
        let data = data.to_vec();
        // write on its own thread: a command that never reads must not block us
        std::thread::spawn(move || {
            let _ = si.write_all(&data);
        });
    }
    let (mut out, mut err) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let o = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = out.read_to_end(&mut b);
        b
    });
    let e = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = err.read_to_end(&mut b);
        b
    });
    let deadline = Instant::now() + TOOL_TIMEOUT;
    let status = loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(s) => break s,
            None if Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{name} took longer than {}s", TOOL_TIMEOUT.as_secs()));
            }
            None => std::thread::sleep(Duration::from_millis(40)),
        }
    };
    let (stdout, stderr) = (o.join().unwrap_or_default(), e.join().unwrap_or_default());
    if status.success() {
        Ok((stdout, stderr))
    } else {
        Err(format!(
            "{name} failed: {}",
            String::from_utf8_lossy(&stderr).trim().chars().take(300).collect::<String>()
        ))
    }
}

/// A language tag like `en-US` or `hu`: letters, digits, `-`, `_`; never an option.
pub fn valid_locale(l: &str) -> bool {
    (2..=12).contains(&l.len())
        && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        && !l.starts_with('-')
}

/// A voice name for `say` (`Samantha`, `Tünde`, `Eddy (English (US))`): never an option.
pub fn valid_voice(v: &str) -> bool {
    !v.is_empty()
        && v.chars().count() <= 48
        && !v.starts_with('-')
        && v.chars().all(|c| c.is_alphanumeric() || " ._()-".contains(c))
}

/// The voice used when none is asked for: by the language of the text, roughly.
pub fn default_voice(locale: &str) -> &'static str {
    match locale.split(['-', '_']).next().unwrap_or("en") {
        "hu" => "Tünde",
        _ => "Samantha",
    }
}

/// Loudness per slice of the audio, scaled so the loudest slice is 1024: the shape a
/// voice message shows. `pcm` is mono signed 16-bit samples.
pub fn waveform(pcm: &[i16], bins: usize) -> Vec<u16> {
    if pcm.is_empty() || bins == 0 {
        return vec![0; bins];
    }
    let per = pcm.len().div_ceil(bins);
    let rms: Vec<f64> = (0..bins)
        .map(|b| {
            let s = &pcm[(b * per).min(pcm.len())..((b + 1) * per).min(pcm.len())];
            if s.is_empty() {
                0.0
            } else {
                (s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len() as f64).sqrt()
            }
        })
        .collect();
    let max = rms.iter().cloned().fold(0.0, f64::max);
    rms.iter().map(|v| if max <= 0.0 { 0 } else { (v / max * 1024.0).round() as u16 }).collect()
}

impl Speech {
    pub fn new(cfg: SpeechConfig) -> Self {
        Self { cfg }
    }

    pub fn config(&self) -> &SpeechConfig {
        &self.cfg
    }

    pub fn can_transcribe(&self) -> bool {
        self.cfg.stt.is_some() && works(&self.cfg.ffmpeg, "-version")
    }

    pub fn can_speak(&self) -> bool {
        (self.cfg.tts.is_some() || (cfg!(target_os = "macos") && works("say", "-v?")))
            && works(&self.cfg.ffmpeg, "-version")
    }

    fn scratch(&self, what: &str) -> Result<tempfile::TempDir, String> {
        tempfile::Builder::new()
            .prefix(&format!("holon-{what}-"))
            .tempdir()
            .map_err(|e| format!("temp dir: {e}"))
    }

    /// Audio in any format ffmpeg reads (an Element voice message, a recording) to text.
    /// An empty string means nothing intelligible was said.
    pub fn transcribe(&self, audio: &[u8], locale: Option<&str>) -> Result<String, String> {
        let stt =
            self.cfg.stt.as_ref().ok_or("speech-to-text is not configured (see --stt-bin)")?;
        if audio.is_empty() {
            return Err("no audio".into());
        }
        if audio.len() > MAX_AUDIO_IN {
            return Err(format!(
                "audio is {} MB; the limit is {} MB",
                audio.len() >> 20,
                MAX_AUDIO_IN >> 20
            ));
        }
        let locale = locale.unwrap_or(&self.cfg.default_locale);
        if !valid_locale(locale) {
            return Err(format!("`{locale}` is not a language tag like en-US"));
        }
        let dir = self.scratch("stt")?;
        let (input, wav) = (dir.path().join("input"), dir.path().join("input.wav"));
        std::fs::write(&input, audio).map_err(|e| e.to_string())?;
        let mut ff = Command::new(&self.cfg.ffmpeg);
        ff.args(["-loglevel", "error", "-y", "-i"])
            .arg(&input)
            .args(["-ar", "16000", "-ac", "1"])
            .arg(&wav);
        run(ff, None).map_err(|e| format!("could not decode the audio: {e}"))?;
        let mut cmd = Command::new(stt);
        cmd.arg(&wav).arg(locale);
        let (out, _) = run(cmd, None)?;
        Ok(String::from_utf8_lossy(&out).trim().to_string())
    }

    /// Text to a voice message: Opus in Ogg, with its length and waveform.
    pub fn speak(&self, text: &str, voice: Option<&str>) -> Result<Spoken, String> {
        let text = text.trim();
        if text.is_empty() {
            return Err("nothing to say".into());
        }
        if text.chars().count() > MAX_TEXT {
            return Err(format!("text is longer than {MAX_TEXT} characters"));
        }
        let voice = voice.unwrap_or_else(|| default_voice(&self.cfg.default_locale));
        if !valid_voice(voice) {
            return Err(format!("`{voice}` is not a voice name"));
        }
        let dir = self.scratch("tts")?;
        let (raw, ogg) = (dir.path().join("speech.aiff"), dir.path().join("speech.ogg"));
        match &self.cfg.tts {
            Some(bin) => {
                let mut c = Command::new(bin);
                c.arg(&raw).arg(voice);
                run(c, Some(text.as_bytes()))?;
            }
            None if cfg!(target_os = "macos") => {
                let mut c = Command::new("say");
                c.args(["-v", voice, "-o"]).arg(&raw).args(["-f", "-"]);
                run(c, Some(text.as_bytes()))?;
            }
            None => {
                return Err("text-to-speech is not available on this machine (see --tts-bin)".into())
            }
        }
        let mut ff = Command::new(&self.cfg.ffmpeg);
        ff.args(["-loglevel", "error", "-y", "-i"])
            .arg(&raw)
            .args(["-c:a", "libopus", "-b:a", "24k", "-ar", "48000", "-ac", "1"])
            .arg(&ogg);
        run(ff, None).map_err(|e| format!("could not encode the speech: {e}"))?;
        let audio = std::fs::read(&ogg).map_err(|e| e.to_string())?;
        // Decode once more, small, for the length and the waveform.
        let mut dec = Command::new(&self.cfg.ffmpeg);
        dec.args(["-loglevel", "error", "-i"])
            .arg(&ogg)
            .args(["-f", "s16le", "-ar", "8000", "-ac", "1", "-"]);
        let (pcm_bytes, _) = run(dec, None)?;
        let pcm: Vec<i16> =
            pcm_bytes.as_chunks::<2>().0.iter().map(|b| i16::from_le_bytes(*b)).collect();
        Ok(Spoken {
            audio,
            duration_ms: pcm.len() as u64 * 1000 / 8000,
            waveform: waveform(&pcm, WAVEFORM_BINS),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales_and_voices_cannot_be_options_or_paths() {
        for ok in ["en-US", "hu", "de_DE", "zh-Hans-CN"] {
            assert!(valid_locale(ok), "{ok}");
        }
        for bad in ["", "e", "-v", "--help", "en US", "../x", "en-US; rm", &"a".repeat(13)] {
            assert!(!valid_locale(bad), "{bad}");
        }
        for ok in ["Samantha", "Tünde", "Eddy (English (US))", "Bad News", "en.voice_1"] {
            assert!(valid_voice(ok), "{ok}");
        }
        for bad in ["", "-o", "--help", "a/b", "x;y", "$(id)", &"v".repeat(49)] {
            assert!(!valid_voice(bad), "{bad}");
        }
    }

    #[test]
    fn the_default_voice_follows_the_language() {
        assert_eq!(default_voice("en-US"), "Samantha");
        assert_eq!(default_voice("hu-HU"), "Tünde");
        assert_eq!(default_voice("hu"), "Tünde");
        assert_eq!(default_voice("zz"), "Samantha");
    }

    #[test]
    fn a_waveform_is_loudness_over_time_scaled_to_the_loudest_slice() {
        let mut pcm = vec![0i16; 1000];
        for (i, s) in pcm.iter_mut().enumerate().skip(500) {
            *s = if i % 2 == 0 { 8000 } else { -8000 };
        }
        let w = waveform(&pcm, 10);
        assert_eq!(w.len(), 10);
        assert!(w[..5].iter().all(|v| *v == 0), "{w:?}");
        assert!(w[5..].iter().all(|v| *v == 1024), "{w:?}");
        assert_eq!(waveform(&[], 4), [0, 0, 0, 0]);
        assert_eq!(waveform(&[0; 50], 5), [0; 5], "silence is flat, not NaN");
        let ramp: Vec<i16> = (0..1000).map(|i| (i * 30) as i16).collect();
        let w = waveform(&ramp, 4);
        assert!(w.windows(2).all(|p| p[0] <= p[1]) && w[3] == 1024, "{w:?}");
    }

    #[test]
    fn unconfigured_engines_say_so_instead_of_failing_strangely() {
        let s = Speech::new(SpeechConfig::default());
        assert!(!s.can_transcribe());
        assert!(s.transcribe(b"x", None).unwrap_err().contains("not configured"));
        assert_eq!(s.speak("  ", None).unwrap_err(), "nothing to say");
        assert!(s.speak(&"a".repeat(MAX_TEXT + 1), None).unwrap_err().contains("longer than"));
        assert!(s.speak("hi", Some("--help")).unwrap_err().contains("not a voice"));
    }

    #[test]
    fn input_is_validated_before_any_command_runs() {
        let s = Speech::new(SpeechConfig {
            stt: Some("/definitely/not/there".into()),
            ..Default::default()
        });
        assert_eq!(s.transcribe(b"", None).unwrap_err(), "no audio");
        assert!(s.transcribe(&vec![0; MAX_AUDIO_IN + 1], None).unwrap_err().contains("limit"));
        assert!(s.transcribe(b"x", Some("--bad")).unwrap_err().contains("language tag"));
    }
}
