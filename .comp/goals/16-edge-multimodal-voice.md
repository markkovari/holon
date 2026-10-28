# Goal 16: Multimodal Voice and Speech for Edge Devices

**Traces to:** ADR-0127.

## What is wanted

1. **Gemini Audio Transcription Adapter**:
   - Implement `transcribe_audio(bytes, mime_type)` in `agent-runtime/src/speech.rs` or `crates/holon-llm` targeting Gemini Flash.
   - Support `audio/wav`, `audio/ogg`, and `audio/mp3` payloads up to 25 MB.
2. **Platform Fallback in `agent-runtime/src/speech.rs`**:
   - Check if `speech/holon-stt.swift` exists and is executable.
   - If running on Linux (e.g. Raspberry Pi Zero 2 W or Pi 5), seamlessly route transcription to Gemini multimodal audio.
3. **Audio Synthesis Fallback**:
   - Implement fallback when `say` is unavailable, generating speech audio via Gemini audio modalities.

## Acceptance Gate

- Test transcribing a 5-second audio snippet on Linux (without `holon-stt.swift` or `say`) successfully returning accurate text.
- Verified on a physical edge node (`rpizero2` or `malna`).
