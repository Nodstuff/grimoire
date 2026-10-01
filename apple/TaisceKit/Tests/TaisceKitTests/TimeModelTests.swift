import Foundation
import Testing
@testable import TaisceKit

/// The device owns local time; the server stores all-day dates and UTC
/// instants. Europe/Dublin's 2026 switches: back on 25 October (02:00 IST →
/// 01:00 GMT), forward on 29 March (01:00 GMT → 02:00 IST).
@Suite struct TimeModelTests {
    static let dublin = TimeZone(identifier: "Europe/Dublin")!
    static let newYork = TimeZone(identifier: "America/New_York")!
    static let kolkata = TimeZone(identifier: "Asia/Kolkata")!

    /// Exact to the second (Deadline.instant truncates to the minute, as the server does).
    static func utc(_ s: String) -> Date { ISO8601DateFormatter().date(from: s)! }

    func instant(_ d: Deadline?) -> Date? {
        if case let .at(t, _) = d { t } else { nil }
    }

    @Test func storedForms() {
        #expect(Deadline(stored: "2026-10-03") == .allDay("2026-10-03"))
        #expect(Deadline(stored: "2026-10-03T14:00Z") == .at(Self.utc("2026-10-03T14:00:00Z")))
        #expect(Deadline(stored: "2026-10-03T15:00:00+01:00") == .at(Self.utc("2026-10-03T14:00:00Z")))
        // pre-UTC wall time: read as UTC, like the server, and flagged
        #expect(Deadline(stored: "2026-10-03 14:00") == .at(Self.utc("2026-10-03T14:00:00Z"), isLegacy: true))
        #expect(Deadline(stored: "2026-02-31") == nil && Deadline(stored: "fri") == nil)
        #expect(Deadline.at(Self.utc("2026-10-03T14:00:00Z")).stored == "2026-10-03T14:00Z")
    }

    @Test func localInputBecomesUTCAcrossTheOctoberSwitch() {
        // the day before: IST (UTC+1)
        #expect(instant(.local(Due("2026-10-24 15:00")!, in: Self.dublin)) == Self.utc("2026-10-24T14:00:00Z"))
        // the day of, after the switch: GMT
        #expect(instant(.local(Due("2026-10-25 15:00")!, in: Self.dublin)) == Self.utc("2026-10-25T15:00:00Z"))
        // 01:30 happens twice that night: the first (IST) wins
        #expect(instant(.local(Due("2026-10-25 01:30")!, in: Self.dublin)) == Self.utc("2026-10-25T00:30:00Z"))
        // and the second 01:30 still reads back as 01:30 local
        #expect(Deadline.at(Self.utc("2026-10-25T01:30:00Z")).due(in: Self.dublin) == Due("2026-10-25 01:30"))
        // a date alone stays a floating date
        #expect(Deadline.local(Due("2026-10-25")!, in: Self.dublin) == .allDay("2026-10-25"))
    }

    @Test func aTimeInTheSpringGapMovesForward() {
        // 01:30 on 29 March doesn't exist in Dublin: 02:30 IST, 01:30Z
        #expect(instant(.local(Due("2026-03-29 01:30")!, in: Self.dublin)) == Self.utc("2026-03-29T01:30:00Z"))
    }

    @Test func allDayIsOverdueAtLocalMidnightEitherSideOfTheSwitch() {
        let sunday = Deadline.allDay("2026-10-25") // GMT by evening
        #expect(!sunday.isOverdue(now: Self.utc("2026-10-25T23:59:00Z"), in: Self.dublin))
        #expect(sunday.isOverdue(now: Self.utc("2026-10-26T00:00:30Z"), in: Self.dublin))
        let saturday = Deadline.allDay("2026-10-24") // IST: midnight is 23:00Z
        #expect(!saturday.isOverdue(now: Self.utc("2026-10-24T22:59:00Z"), in: Self.dublin))
        #expect(saturday.isOverdue(now: Self.utc("2026-10-24T23:00:30Z"), in: Self.dublin))
        // the 09:00 local alert, on each side of the switch
        #expect(saturday.alertDate(in: Self.dublin) == Self.utc("2026-10-24T08:00:00Z"))
        #expect(sunday.alertDate(in: Self.dublin) == Self.utc("2026-10-25T09:00:00Z"))
    }

    @Test func aTimedDeadlineIsOverdueByItsInstantOnly() {
        let d = Deadline.local(Due("2026-10-25 15:00")!, in: Self.dublin)!
        #expect(!d.isOverdue(now: Self.utc("2026-10-25T14:59:00Z"), in: Self.dublin))
        #expect(d.isOverdue(now: Self.utc("2026-10-25T15:00:30Z"), in: Self.dublin))
        #expect(d.alertDate(in: Self.newYork) == Self.utc("2026-10-25T15:00:00Z"), "an instant alerts at the instant anywhere")
    }

    /// Written in Dublin, then the phone flies to New York.
    @Test func travelKeepsInstantsAndFloatsDates() {
        let meeting = Deadline.local(Due("2026-10-03 15:00")!, in: Self.dublin)!
        #expect(meeting.wire.dueAt == "2026-10-03T14:00:00Z")
        #expect(meeting.due(in: Self.newYork) == Due("2026-10-03 10:00"), "shown in the zone the phone is in")
        #expect(meeting.day(in: Self.kolkata) == "2026-10-03")
        for zone in [Self.dublin, Self.newYork] {
            #expect(!meeting.isOverdue(now: Self.utc("2026-10-03T13:59:00Z"), in: zone))
            #expect(meeting.isOverdue(now: Self.utc("2026-10-03T14:00:30Z"), in: zone))
        }
        // an all-day date is due on that date wherever the phone is
        let rent = Deadline.allDay("2026-10-03")
        #expect(rent.isOverdue(now: Self.utc("2026-10-03T23:00:30Z"), in: Self.dublin))
        #expect(!rent.isOverdue(now: Self.utc("2026-10-04T03:59:00Z"), in: Self.newYork))
        #expect(rent.isOverdue(now: Self.utc("2026-10-04T04:00:30Z"), in: Self.newYork))
        #expect(rent.alertDate(in: Self.newYork) == Self.utc("2026-10-03T13:00:00Z"), "09:00 EDT")
        // a late-evening instant falls on the next day further east
        let late = Deadline.at(Self.utc("2026-10-03T22:30:00Z"))
        #expect(late.day(in: Self.dublin) == "2026-10-03" && late.day(in: Self.kolkata) == "2026-10-04")
    }

    @Test func theClockSentWithEveryCall() {
        func clock(_ now: String, _ zone: TimeZone) -> [String] {
            let c = TodoClock(now: Self.utc(now), timeZone: zone)
            return [c.today, c.utcOffset]
        }
        #expect(clock("2026-07-01T12:00:00Z", Self.dublin) == ["2026-07-01", "+01:00"])
        #expect(clock("2026-12-01T12:00:00Z", Self.dublin) == ["2026-12-01", "+00:00"])
        #expect(clock("2026-10-04T02:00:00Z", Self.newYork) == ["2026-10-03", "-04:00"])
        #expect(clock("2026-10-03T20:00:00Z", Self.kolkata) == ["2026-10-04", "+05:30"])
        #expect(TodoClock.fixed("2026-10-04", "+05:30").timeZone.secondsFromGMT() == 19800)
    }

    @Test func deadlineWritesSendDeadlineOrDueAtPlusToday() async throws {
        let server = MockServer { _ in .json(#"{"carried":0,"date":"2026-10-01","doc_id":"t","epoch":3,"items":[],"prev_date":null,"today":"2026-10-01"}"#) }
        let api = server.client()
        let clock = TodoClock.fixed("2026-10-01", "+01:00")
        _ = try await api.todoSetDeadline(date: "2026-10-01", itemID: "0-ab", deadline: .local(Due("2026-10-03 15:00")!, in: Self.dublin), clock: clock)
        _ = try await api.todoSetDeadline(date: "2026-10-01", itemID: "0-ab", deadline: .allDay("2026-10-04"), clock: clock)
        _ = try await api.todoSetDeadline(date: "2026-10-01", itemID: "0-ab", deadline: nil as Deadline?, clock: clock)
        let bodies = try server.requests.map { try #require(try JSONSerialization.jsonObject(with: $0.httpBody ?? Data()) as? [String: String?]) }
        #expect(bodies[0]["due_at"] == "2026-10-03T14:00:00Z")
        // the local date rides along, so a pre-UTC server still lands on the right day
        #expect(bodies[0]["deadline"] == "2026-10-03")
        #expect(bodies[1]["deadline"] == "2026-10-04" && bodies[1]["due_at"] == nil)
        #expect(bodies[2]["deadline"] == .some(nil) && bodies[2]["due_at"] == nil)
        #expect(bodies.allSatisfy { $0["today"] == "2026-10-01" && $0["item_id"] == "0-ab" })
        #expect(bodies.allSatisfy { $0["due_time"] == nil }, "no wall times on the wire")
    }

    @Test func dueListDecodesBothShapesAndComputesOverdueLocally() throws {
        let new = """
        {"doc_id":"t","epoch":3,"now":"2026-10-03T12:00:00Z","until":null,"default_alert_time":"09:00","items":[
          {"date":"2026-10-01","id":"0-a","text":"meeting","due_at":"2026-10-03T14:00:00Z"},
          {"date":"2026-10-01","id":"1-b","text":"rent","deadline":"2026-10-02"},
          {"date":"2026-10-01","id":"2-c","text":"old","due_at":"2026-10-03T09:00:00Z","legacy_time":true}]}
        """
        let list = try JSONDecoder().decode(TodoDueList.self, from: Data(new.utf8))
        let now = Self.utc("2026-10-03T12:00:00Z")
        #expect(list.items.map { $0.deadlineValue?.isOverdue(now: now, in: Self.dublin) } == [false, true, true])
        #expect(list.items[0].deadlineValue == .at(Self.utc("2026-10-03T14:00:00Z")))
        #expect(list.items[2].deadlineValue == .at(Self.utc("2026-10-03T09:00:00Z"), isLegacy: true))
        #expect(list.items[1].deadline == "2026-10-02" && list.items[1].dueTime == nil)
        // the old shape still decodes; its server-computed `overdue` is ignored
        let old = """
        {"doc_id":"t","epoch":3,"now":"2026-10-03T12:00","until":null,"default_alert_time":"09:00","items":[
          {"date":"2026-10-01","id":"0-a","text":"x","deadline":"2099-01-01","due_time":"10:00","alert_at":"2099-01-01T10:00","overdue":true}]}
        """
        let o = try JSONDecoder().decode(TodoDueList.self, from: Data(old.utf8))
        #expect(o.items.first?.deadlineValue?.isOverdue(now: now) == false)
        #expect(o.items.first?.overdue == false)
    }

    @Test func todoItemsReadDueAt() throws {
        let json = #"{"id":"0-a","text":"meeting","done":false,"due_at":"2026-10-03T14:00:00Z","due_soon":true}"#
        let item = try JSONDecoder().decode(TodoItem.self, from: Data(json.utf8))
        #expect(item.deadlineValue == .at(Self.utc("2026-10-03T14:00:00Z")))
        #expect(item.deadlineValue?.due(in: Self.dublin) == Due("2026-10-03 15:00"))
    }

    @Test func theOfflineParserReadsUTCInstants() {
        let items = TodoParser.parse(markdown: """
        ## 2026-10-03

        - [ ] meeting · due 2026-10-03T14:00Z

        - [ ] rent · due 2026-10-02

        - [ ] legacy · due 2026-10-03 09:00
        """)
        #expect(items.map(\.text) == ["meeting", "rent", "legacy"])
        #expect(items[0].deadlineValue == .at(Self.utc("2026-10-03T14:00:00Z")))
        #expect(items[1].deadlineValue == .allDay("2026-10-02"))
        #expect(items[2].deadlineValue == .at(Self.utc("2026-10-03T09:00:00Z"), isLegacy: true))
        // the Today list, from the cache, at 12:00Z on the 3rd in Dublin
        let due = TodoParser.dueOrOverdue(items, now: Self.utc("2026-10-03T12:00:00Z"), timeZone: Self.dublin)
        #expect(due.map(\.text) == ["rent", "legacy", "meeting"], "soonest first; all three are due today or earlier")
    }
}

extension TodoClock {
    static func fixed(_ today: String, _ offset: String) -> TodoClock { TodoClock(today: today, utcOffset: offset) }
}
