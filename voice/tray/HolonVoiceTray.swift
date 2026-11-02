import AppKit
import Foundation

class AppDelegate: NSObject, NSApplicationDelegate {
    var statusItem: NSStatusItem!
    var pollTimer: Timer?
    var animTimer: Timer?
    var animFrame: Int = 0

    var isListening: Bool = false
    var isConnected: Bool = false
    var currentAgent: String = "coach"
    var activeAgent: String? = nil
    var currentState: String = "Paused"
    var lastHeard: String = ""
    var lastReply: String = ""
    var activeDevice: String = ""
    let controlUrl = URL(string: "http://127.0.0.1:18018")!

    // Animation sequences
    let waveFrames = [
        " ▂▃", "▂▃▅", "▃▅▆", "▅▆▇", "▆▇█",
        "▇█▇", "█▇▆", "▇▆▅", "▆▅▃", "▅▃▂"
    ]
    let spinnerFrames = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"]
    let pulseFrames = ["∿", "≋", "≈", "≋"]

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Run purely as menu bar accessory (no Dock icon)
        NSApp.setActivationPolicy(.accessory)

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        updateStatusDisplay()
        buildMenu()

        // Poll status every 400ms for responsive state updates
        pollTimer = Timer.scheduledTimer(withTimeInterval: 0.4, repeats: true) { [weak self] _ in
            self?.pollStatus()
        }

        // Animation frame timer (runs at 120ms for smooth 8fps animation)
        animTimer = Timer.scheduledTimer(withTimeInterval: 0.12, repeats: true) { [weak self] _ in
            self?.tickAnimation()
        }

        pollStatus()
    }

    func tickAnimation() {
        animFrame = (animFrame + 1) % 120
        // Only trigger UI update if an animated state is active
        if isConnected && (currentState == "Processing" || currentState == "Speaking" || currentState == "HearingSpeech") {
            updateStatusDisplay()
        }
    }

    func updateStatusDisplay() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self, let button = self.statusItem.button else { return }

            if !self.isConnected {
                button.title = "⚠️ Holon"
                return
            }

            if !self.isListening {
                button.title = "🔇 Holon"
                return
            }

            let agentDisplayName = (self.activeAgent ?? self.currentAgent).capitalized

            switch self.currentState {
            case "Speaking":
                // Equalizer wave animation when an agent is talking
                let wave = self.waveFrames[self.animFrame % self.waveFrames.count]
                button.title = "🔊 \(wave) \(agentDisplayName)"
            case "Processing":
                // Spinner animation when an agent is thinking / querying Concept2 / LLM
                let spinner = self.spinnerFrames[self.animFrame % self.spinnerFrames.count]
                button.title = "💭 \(spinner) \(agentDisplayName)"
            case "HearingSpeech":
                // Pulse animation while user speech is detected
                let pulse = self.pulseFrames[self.animFrame % self.pulseFrames.count]
                button.title = "🎙️ \(pulse) Listening"
            default:
                // Idle listening for wake-word
                button.title = "🎙️ Holon"
            }
        }
    }

    func buildMenu() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            let menu = NSMenu()

            // Header title
            let header = NSMenuItem(title: "Holon Voice Companion", action: nil, keyEquivalent: "")
            header.isEnabled = false
            menu.addItem(header)

            // Current State with rich indicators
            let stateTitle: String
            let agentDisplayName = (self.activeAgent ?? self.currentAgent).capitalized

            if !self.isConnected {
                stateTitle = "⚠️ Service Disconnected"
            } else if !self.isListening {
                stateTitle = "⚪ Status: Muted (Microphone Released)"
            } else {
                switch self.currentState {
                case "Speaking":
                    stateTitle = "🔊 Status: \(agentDisplayName) is Speaking..."
                case "Processing":
                    stateTitle = "🟡 Status: \(agentDisplayName) is Thinking..."
                case "HearingSpeech":
                    stateTitle = "🎙️ Status: Hearing Your Voice..."
                default:
                    stateTitle = "🟢 Status: Listening (Say 'Hey Holon' or '\(agentDisplayName)')"
                }
            }
            let stateItem = NSMenuItem(title: stateTitle, action: nil, keyEquivalent: "")
            stateItem.isEnabled = false
            menu.addItem(stateItem)

            menu.addItem(NSMenuItem.separator())

            // Primary Toggle
            let toggleTitle = self.isListening ? "Turn Microphone Off (Mute)" : "Turn Microphone On (Listen)"
            let toggleItem = NSMenuItem(title: toggleTitle, action: #selector(self.toggleListening), keyEquivalent: "t")
            toggleItem.target = self
            toggleItem.isEnabled = self.isConnected
            menu.addItem(toggleItem)

            menu.addItem(NSMenuItem.separator())

            // Target Agent submenu
            let agentMenu = NSMenu()
            let availableAgents = [
                ("coach", "Coach (Concept2 Rowing)"),
                ("bot", "Bot (General Assistant)"),
                ("calendar", "Calendar (Scheduling)"),
                ("weather", "Weather (Forecasts)"),
                ("pr-bot", "PR Bot (Code Reviews)"),
            ]
            for (id, label) in availableAgents {
                let item = NSMenuItem(title: label, action: #selector(self.selectAgent(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = id
                if id == self.currentAgent {
                    item.state = .on
                }
                agentMenu.addItem(item)
            }
            let agentParent = NSMenuItem(title: "Default Agent: \(self.currentAgent.capitalized)", action: nil, keyEquivalent: "")
            agentParent.submenu = agentMenu
            agentParent.isEnabled = self.isConnected
            menu.addItem(agentParent)

            menu.addItem(NSMenuItem.separator())

            // Device info
            if !self.activeDevice.isEmpty {
                let deviceItem = NSMenuItem(title: "Input: \(self.activeDevice)", action: nil, keyEquivalent: "")
                deviceItem.isEnabled = false
                menu.addItem(deviceItem)
            }

            // Recent activity
            if !self.lastHeard.isEmpty {
                let heardItem = NSMenuItem(title: "Heard: \"\(self.lastHeard)\"", action: nil, keyEquivalent: "")
                heardItem.isEnabled = false
                menu.addItem(heardItem)
            }

            if !self.lastReply.isEmpty {
                let shortReply = self.lastReply.prefix(50) + (self.lastReply.count > 50 ? "..." : "")
                let replyItem = NSMenuItem(title: "Reply: \"\(shortReply)\"", action: nil, keyEquivalent: "")
                replyItem.isEnabled = false
                menu.addItem(replyItem)
            }

            if !self.lastHeard.isEmpty || !self.lastReply.isEmpty {
                menu.addItem(NSMenuItem.separator())
            }

            let logsItem = NSMenuItem(title: "Open Voice Logs...", action: #selector(self.openLogs), keyEquivalent: "l")
            logsItem.target = self
            menu.addItem(logsItem)

            let quitItem = NSMenuItem(title: "Quit Tray", action: #selector(self.quitApp), keyEquivalent: "q")
            quitItem.target = self
            menu.addItem(quitItem)

            self.statusItem.menu = menu
        }
    }

    func pollStatus() {
        let url = controlUrl.appendingPathComponent("status")
        var request = URLRequest(url: url)
        request.timeoutInterval = 0.5

        let task = URLSession.shared.dataTask(with: request) { [weak self] data, response, error in
            guard let self = self else { return }
            if let data = data,
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                let listening = (json["listening"] as? Bool) ?? false
                let agent = (json["agent"] as? String) ?? "coach"
                let state = (json["state"] as? String) ?? "Paused"
                let activeAgent = json["active_agent"] as? String
                let device = (json["device"] as? String) ?? ""
                let heard = (json["last_heard"] as? String) ?? ""
                let reply = (json["last_reply"] as? String) ?? ""

                let stateChanged = (self.isListening != listening)
                    || (self.isConnected != true)
                    || (self.currentState != state)
                    || (self.activeAgent != activeAgent)
                    || (self.currentAgent != agent)
                    || (self.lastHeard != heard)
                    || (self.lastReply != reply)

                self.isConnected = true
                self.isListening = listening
                self.currentState = state
                self.activeAgent = activeAgent
                self.currentAgent = agent
                self.activeDevice = device
                self.lastHeard = heard
                self.lastReply = reply

                self.updateStatusDisplay()
                if stateChanged {
                    self.buildMenu()
                }
            } else {
                if self.isConnected {
                    self.isConnected = false
                    self.updateStatusDisplay()
                    self.buildMenu()
                }
            }
        }
        task.resume()
    }

    @objc func toggleListening() {
        postEndpoint("toggle")
    }

    @objc func selectAgent(_ sender: NSMenuItem) {
        guard let agent = sender.representedObject as? String else { return }
        postEndpoint("agent?name=\(agent)")
    }

    @objc func openLogs() {
        let home = FileManager.default.homeDirectoryForCurrentUser
        let logUrl = home.appendingPathComponent("Library/Logs/holon/voice.log")
        NSWorkspace.shared.open(logUrl)
    }

    @objc func quitApp() {
        NSApplication.shared.terminate(nil)
    }

    func postEndpoint(_ path: String) {
        guard let url = URL(string: "http://127.0.0.1:18018/\(path)") else { return }
        var req = URLRequest(url: url)
        req.httpMethod = "POST"
        req.timeoutInterval = 2.0
        let task = URLSession.shared.dataTask(with: req) { [weak self] data, _, _ in
            self?.pollStatus()
        }
        task.resume()
    }
}

let app = NSApplication.shared
let delegate = AppDelegate()
app.delegate = delegate
app.run()
