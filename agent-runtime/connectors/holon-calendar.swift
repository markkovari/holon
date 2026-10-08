// holon-calendar: the Calendar connector for the agent runtime, over EventKit.
//
// EventKit reads every calendar the Mac has — iCloud, Google, Exchange, subscribed — so
// adding a Google account in System Settings > Internet Accounts (Calendars on) makes it
// available here, with no OAuth app of our own.
//
//   holon-calendar calendars                       stdin ignored
//   holon-calendar list     {"from":"2026-10-06","to":"2026-10-13","calendar":"Work"}
//   holon-calendar add      {"title":"Row","start":"2026-10-07T06:30","end":"2026-10-07T07:30",
//                            "calendar":"Home","notes":"UT2"}
//
// Dates are ISO 8601 in local time ("2026-10-07" or "2026-10-07T06:30"). Output is plain text.
// First run asks macOS for Calendar access: run it once from a terminal and allow it.
//
// Build:  swiftc -parse-as-library -O -o holon-calendar holon-calendar.swift \
//           -Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker calendar-info.plist

import EventKit
import Foundation

func fail(_ message: String, _ code: Int32 = 1) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(code)
}

func date(_ s: String?, endOfDay: Bool = false) -> Date? {
    guard let s, !s.isEmpty else { return nil }
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.timeZone = TimeZone.current
    for fmt in ["yyyy-MM-dd'T'HH:mm:ss", "yyyy-MM-dd'T'HH:mm", "yyyy-MM-dd"] {
        f.dateFormat = fmt
        if let d = f.date(from: s) {
            return fmt == "yyyy-MM-dd" && endOfDay ? d.addingTimeInterval(86_399) : d
        }
    }
    return nil
}

func show(_ d: Date, allDay: Bool) -> String {
    let f = DateFormatter()
    f.locale = Locale(identifier: "en_US_POSIX")
    f.dateFormat = allDay ? "yyyy-MM-dd" : "yyyy-MM-dd HH:mm"
    return f.string(from: d)
}

@main
struct Main {
    static func main() async {
        let args = CommandLine.arguments
        guard args.count >= 2 else { fail("usage: holon-calendar calendars|list|add  (JSON on stdin)", 2) }
        let input = FileHandle.standardInput.readDataToEndOfFile()
        let json = (try? JSONSerialization.jsonObject(with: input)) as? [String: Any] ?? [:]
        let store = EKEventStore()
        do {
            guard try await store.requestFullAccessToEvents() else {
                fail("Calendar access was not granted. Allow it in System Settings > Privacy & Security > Calendars.", 3)
            }
        } catch { fail("Calendar access failed: \(error.localizedDescription)", 3) }

        func calendars(named name: String?) -> [EKCalendar]? {
            guard let name, !name.isEmpty else { return nil }
            let found = store.calendars(for: .event).filter { $0.title.lowercased() == name.lowercased() }
            if found.isEmpty { fail("no calendar named \(name). Use `calendars` to list them.") }
            return found
        }

        switch args[1] {
        case "calendars":
            for c in store.calendars(for: .event) {
                print("\(c.title) (\(c.source.title)\(c.allowsContentModifications ? "" : ", read-only"))")
            }
        case "list":
            let from = date(json["from"] as? String) ?? Date()
            let to = date(json["to"] as? String, endOfDay: true) ?? from.addingTimeInterval(7 * 86_400)
            if to < from { fail("`to` is before `from`") }
            let pred = store.predicateForEvents(withStart: from, end: to, calendars: calendars(named: json["calendar"] as? String))
            let events = store.events(matching: pred).sorted { $0.startDate < $1.startDate }.prefix(60)
            if events.isEmpty { print("no events") }
            for e in events {
                let when = e.isAllDay
                    ? "\(show(e.startDate, allDay: true)) (all day)"
                    : "\(show(e.startDate, allDay: false))–\(show(e.endDate, allDay: false).suffix(5))"
                let place = (e.location ?? "").isEmpty ? "" : " @ \(e.location!)"
                print("\(when)  \(e.title ?? "(no title)")\(place)  [\(e.calendar.title)]")
            }
        case "add":
            guard let title = json["title"] as? String, !title.isEmpty,
                  let start = date(json["start"] as? String) else { fail("`title` and a valid `start` are required", 2) }
            let end = date(json["end"] as? String) ?? start.addingTimeInterval(3600)
            let ev = EKEvent(eventStore: store)
            ev.title = title
            ev.startDate = start
            ev.endDate = end
            ev.notes = json["notes"] as? String
            ev.location = json["location"] as? String
            ev.calendar = calendars(named: json["calendar"] as? String)?.first ?? store.defaultCalendarForNewEvents
            if let cal = ev.calendar, !cal.allowsContentModifications { fail("calendar \(cal.title) is read-only") }
            do { try store.save(ev, span: .thisEvent) } catch { fail("could not save: \(error.localizedDescription)") }
            print("added \(show(start, allDay: false)) \(title) to \(ev.calendar.title)")
        default:
            fail("unknown command \(args[1])", 2)
        }
    }
}
