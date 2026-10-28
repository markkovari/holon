# ADR-0127 — Multimodal Voice and Speech for Edge Devices

*The current speech implementation relies on macOS-specific binaries (`holon-stt.swift`, `say`) and `ffmpeg`, leaving Linux edge devices (Raspberry Pi Zero 2 W, Pi 5) without voice capabilities; adopting Gemini's multimodal audio API provides universal speech-to-text and text-to-speech with zero local native tool dependencies.*

**Status: accepted.**

## The problem

1. **Host-platform lock-in**: `agent-runtime/src/speech.rs` invokes `speech/holon-stt.swift` (Apple's on-device Speech framework) for transcription and macOS `say` for synthesis. This works only on macOS.
2. **Edge resource constraints**: On a Raspberry Pi Zero 2 W (512 MB RAM), compiling or running local `whisper.cpp` or Piper TTS models exhausts memory and CPU, risking OOM kills.
3. **External dependencies**: Running `ffmpeg` for Opus/Ogg transcoding requires system package installation on every node.

## The decision

1. **Multimodal Audio input via Gemini**:
   - For STT, send raw recorded audio directly as an inline data part (`audio/wav`, `audio/mp3`, `audio/ogg`) in Gemini's multimodal request.
   - Prompt Gemini for exact transcription with timestamps and speaker diarization.
   - This runs completely free under Gemini Flash's free tier without local native binaries.
2. **Audio synthesis via Gemini Live / TTS**:
   - Request audio modality outputs (`modalities: ["AUDIO", "TEXT"]`) or pipe text through lightweight cloud TTS when running on non-Apple edge nodes.
3. **Pluggable Speech Engine Contract in `speech.rs`**:
   - `speech.rs` checks for local Apple Speech on macOS; if absent (Linux / edge), it delegates to the Gemini multimodal voice pipeline.

## What it does not do

- On macOS workstations, offline Apple Speech (`holon-stt.swift`) remains preferred for instant, zero-network private transcription.
- The voice format remains standard Opus in Ogg for chat compatibility.
