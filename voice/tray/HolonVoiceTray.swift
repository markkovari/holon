import AppKit
import Foundation

class AppDelegate: NSObject, NSApplicationDelegate {
    var statusItem: NSStatusItem!
    var timer: Timer?
    var isListening: Bool = false
    var isConnected: Bool = false
    var currentAgent: String = "coach"
    var lastHeard: String = ""
    var lastReply: String = ""
    var activeDevice: String = ""
    let controlUrl = URL(string: "http://127.0.0.1:18018")!

    func applicationDidFinishLaunching(_ notification: Notification) {
        // Prevent app from showing in Dock
        NSApp.setActivationPolicy(.accessory)

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        updateStatusDisplay()
        buildMenu()

        timer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
            self?.pollStatus()
        }
        pollStatus()
    }

    func updateStatusDisplay() {
        DispatchQueue.main.async { [weak self] in
            guard let self = self, let button = self.statusItem.button else { return }
            if !self.isConnected {
                button.title = "⚠️ Holon"
            } else if self.isListening {
                button.title = "🎙️ Holon"
            } else {
                button.title = "🔇 Holon"
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

            // Current State
            let stateTitle: String
            if !self.isConnected {
                stateTitle = "⚠️ Service Disconnected"
            } else if self.isListening {
                stateTitle = "🟢 Status: Listening (Hey Holon / Coach)"
            } else {
                stateTitle = "⚪ Status: Muted (Microphone Released)"
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
            let agentParent = NSMenuItem(title: "Active Agent: \(self.currentAgent.capitalized)", action: nil, keyEquivalent: "")
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
        request.timeoutInterval = 1.0

        let task = URLSession.shared.dataTask(with: request) { [weak self] data, response, error in
            guard let self = self else { return }
            if let data = data,
               let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any] {
                let listening = (json["listening"] as? Bool) ?? false
                let agent = (json["agent"] as? String) ?? "coach"
                let device = (json["device"] as? String) ?? ""
                let heard = (json["last_heard"] as? String) ?? ""
                let reply = (json["last_reply"] as? String) ?? ""

                let stateChanged = (self.isListening != listening)
                    || (self.isConnected != true)
                    || (self.currentAgent != agent)
                    || (self.lastHeard != heard)
                    || (self.lastReply != reply)

                self.isConnected = true
                self.isListening = listening
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
