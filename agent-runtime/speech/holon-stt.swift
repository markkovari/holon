// holon-stt: transcribe an audio file with Apple's ON-DEVICE speech recognizer.
//
//   holon-stt <audio file> [locale, default en-US]   -> transcript on stdout, nothing else
//
// Diagnostics go to stderr. Needs macOS 26+ (SpeechTranscriber). It runs headless — no
// permission prompt, no network — and uses the locale's on-device model, which macOS
// downloads once if it is not installed yet. Locales without a model (Apple has none for
// Hungarian) exit 4 with the supported list on stderr: use another engine for those
// (any command with the same contract: `<bin> <audio.wav> <locale>` prints the transcript).
//
// Build:  swiftc -parse-as-library -O -o holon-stt holon-stt.swift
import AVFoundation
import Foundation
import Speech

@main
struct HolonSTT {
    static func fail(_ message: String, _ code: Int32) -> Never {
        FileHandle.standardError.write((message + "\n").data(using: .utf8)!)
        exit(code)
    }

    static func main() async {
        let args = CommandLine.arguments
        guard args.count > 1 else { fail("usage: holon-stt <audio file> [locale]", 2) }
        guard #available(macOS 26.0, *) else { fail("holon-stt needs macOS 26 or newer", 3) }
        let locale = Locale(identifier: args.count > 2 ? args[2] : "en-US")

        let supported = await SpeechTranscriber.supportedLocales
        guard supported.contains(where: { $0.identifier(.bcp47) == locale.identifier(.bcp47) }) else {
            let names = supported.map { $0.identifier(.bcp47) }.sorted().joined(separator: ", ")
            fail("no on-device speech model for \(locale.identifier). Supported: \(names)", 4)
        }

        let transcriber = SpeechTranscriber(
            locale: locale, transcriptionOptions: [], reportingOptions: [], attributeOptions: [])

        // The model is downloaded once per locale if it is not already installed.
        let installed = await SpeechTranscriber.installedLocales
        if !installed.contains(where: { $0.identifier(.bcp47) == locale.identifier(.bcp47) }) {
            do {
                if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
                    try await request.downloadAndInstall()
                }
            } catch { fail("could not install the \(locale.identifier) speech model: \(error)", 5) }
        }

        do {
            let file = try AVAudioFile(forReading: URL(fileURLWithPath: args[1]))
            let analyzer = SpeechAnalyzer(modules: [transcriber])
            let collector = Task { () -> String in
                var text = ""
                do {
                    for try await result in transcriber.results where result.isFinal {
                        text += String(result.text.characters)
                    }
                } catch { FileHandle.standardError.write("results: \(error)\n".data(using: .utf8)!) }
                return text
            }
            try await analyzer.start(inputAudioFile: file, finishAfterFile: true)
            print(await collector.value.trimmingCharacters(in: .whitespacesAndNewlines))
        } catch { fail("transcription failed: \(error)", 1) }
    }
}
