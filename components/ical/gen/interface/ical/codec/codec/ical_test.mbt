test "format_event basic" {
  let ev : @codec.Event = {
    uid: "test-uid-123",
    start: 1784624400UL, // 2026-07-21 09:00:00 UTC
    end: 1784626200UL,   // 2026-07-21 09:30:00 UTC
    summary: "Team Sync",
    description: "Weekly sync meeting",
    location: "Conference Room B",
    organizer: "lead@example.com",
    rrule: "",
    alarm_minutes: 15U,
  }
  let ics = @codec.format_event(ev, "acme//cal")
  assert_eq(ics.has_prefix("BEGIN:VCALENDAR\r\n"), true)
  assert_eq(ics.contains("PRODID:-//acme//cal//EN\r\n"), true)
  assert_eq(ics.contains("UID:test-uid-123\r\n"), true)
  assert_eq(ics.contains("DTSTART:20260721T090000Z\r\n"), true)
  assert_eq(ics.contains("DTEND:20260721T093000Z\r\n"), true)
  assert_eq(ics.contains("SUMMARY:Team Sync\r\n"), true)
  assert_eq(ics.contains("DESCRIPTION:Weekly sync meeting\r\n"), true)
  assert_eq(ics.contains("LOCATION:Conference Room B\r\n"), true)
  assert_eq(ics.contains("ORGANIZER:mailto:lead@example.com\r\n"), true)
  assert_eq(ics.contains("BEGIN:VALARM\r\n"), true)
  assert_eq(ics.contains("TRIGGER:-PT15M\r\n"), true)
  assert_eq(ics.contains("END:VALARM\r\n"), true)
  assert_eq(ics.has_suffix("END:VCALENDAR\r\n"), true)
}

test "format_calendar with multiple events and escaping" {
  let ev1 : @codec.Event = {
    uid: "ev-1",
    start: 1784624400UL,
    end: 1784624400UL, // end == start: 0-length
    summary: "Meeting; Urgent, Important \\ Note\nNext line",
    description: "",
    location: "",
    organizer: "",
    rrule: "FREQ=WEEKLY;COUNT=4",
    alarm_minutes: 0U,
  }
  let ev2 : @codec.Event = {
    uid: "ev-2",
    start: 1784628000UL,
    end: 1784631600UL,
    summary: "Simple Event",
    description: "",
    location: "",
    organizer: "",
    rrule: "",
    alarm_minutes: 0U,
  }
  let ics = @codec.format_calendar([ev1, ev2], "", "Company Calendar")
  assert_eq(ics.contains("X-WR-CALNAME:Company Calendar\r\n"), true)
  assert_eq(ics.contains("PRODID:-//comp//ical//EN\r\n"), true)
  assert_eq(ics.contains("SUMMARY:Meeting\\; Urgent\\, Important \\\\ Note\\nNext line\r\n"), true)
  assert_eq(ics.contains("RRULE:FREQ=WEEKLY;COUNT=4\r\n"), true)
  assert_eq(ics.contains("UID:ev-1\r\n"), true)
  assert_eq(ics.contains("UID:ev-2\r\n"), true)
}

test "line folding at 75 octets" {
  let long_desc = "This is a very long description that definitely exceeds seventy-five octets and therefore must be folded according to RFC 5545 specifications onto multiple lines with CRLF and space."
  let ev : @codec.Event = {
    uid: "fold-test",
    start: 1784624400UL,
    end: 1784628000UL,
    summary: "Folding Test",
    description: long_desc,
    location: "",
    organizer: "",
    rrule: "",
    alarm_minutes: 0U,
  }
  let ics = @codec.format_event(ev, "")
  assert_eq(ics.contains("\r\n "), true)
  // Ensure lines never exceed 75 octets excluding CRLF
  for line in ics.split("\r\n") {
    let mut octets = 0
    for c in line {
      let cp = c.to_int()
      let clen = if cp <= 0x7F { 1 } else if cp <= 0x7FF { 2 } else if cp <= 0xFFFF { 3 } else { 4 }
      octets = octets + clen
    }
    assert_eq(octets <= 75, true)
  }
}
