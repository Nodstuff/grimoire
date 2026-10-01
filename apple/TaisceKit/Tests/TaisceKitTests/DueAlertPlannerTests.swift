import Foundation
import Testing
@testable import TaisceKit

@Suite struct DueAlertPlannerTests {
    let dublin = TimeZone(identifier: "Europe/Dublin")!
    let newYork = TimeZone(identifier: "America/New_York")!

    func at(_ s: String) throws -> Date { try #require(DueAlertInput.parseInstant(s)) }

    func timed(_ id: String, _ instant: String, done: Bool = false) throws -> DueAlertInput {
        DueAlertInput(day: "2026-10-01", itemID: id, title: "item \(id)", docTitle: "To-do", kind: .timed(try at(instant)), done: done)
    }

    func allDay(_ id: String, _ y: Int, _ m: Int, _ d: Int, done: Bool = false) -> DueAlertInput {
        DueAlertInput(day: "2026-10-01", itemID: id, title: "item \(id)", docTitle: "To-do", kind: .allDay(DateComponents(year: y, month: m, day: d)), done: done)
    }

    @Test func timedFiresAtItsInstantWithLocalTimeInTheBody() throws {
        let plan = DueAlertPlanner.plan([try timed("0-a", "2026-10-10T14:00:00Z")], timeZone: dublin, now: try at("2026-10-01T12:00:00Z"))
        let a = try #require(plan.first)
        #expect(a.identifier == "todo:2026-10-01/0-a")
        #expect(a.title == "item 0-a")
        #expect(a.body == "Due 15:00 · To-do")
        #expect(a.fireDate == (try at("2026-10-10T14:00:00Z")))
        #expect(a.components.hour == 15 && a.components.minute == 0 && a.components.timeZone == dublin)
    }

    @Test func allDayFiresAtNineLocalAndFloats() throws {
        let plan = DueAlertPlanner.plan([allDay("1-b", 2026, 10, 10)], timeZone: dublin, now: try at("2026-10-01T12:00:00Z"))
        let a = try #require(plan.first)
        #expect(a.body == "Due today · To-do")
        #expect(a.fireDate == (try at("2026-10-10T08:00:00Z")))  // 09:00 IST
        #expect(a.components == DateComponents(year: 2026, month: 10, day: 10, hour: 9, minute: 0))
        #expect(a.components.timeZone == nil)
    }

    /// 01:30 on 2026-10-25 happens twice in Dublin (IST, then GMT): a wall
    /// time typed on the device alerts at the first one, 00:30Z, and the
    /// body still says 01:30.
    @Test func repeatedHourInDublinAlertsAtTheFirstOccurrence() throws {
        let input = try #require(DueAlertInput(
            day: "2026-10-25", itemID: "0-r", title: "repeated", docTitle: "To-do",
            deadline: "2026-10-25", dueTime: "01:30", timeZone: dublin
        ))
        #expect(input.kind == .timed(try at("2026-10-25T00:30:00Z")))
        let plan = DueAlertPlanner.plan([input], timeZone: dublin, now: try at("2026-10-24T12:00:00Z"))
        let a = try #require(plan.first)
        #expect(a.fireDate == (try at("2026-10-25T00:30:00Z")))
        #expect(a.body == "Due 01:30 · To-do")
    }

    /// 01:30 on 2026-03-29 never happens in Dublin (01:00 GMT jumps to
    /// 02:00 IST): the alert moves forward by the gap, to 02:30 IST = 01:30Z.
    @Test func skippedHourInDublinMovesForward() throws {
        let input = try #require(DueAlertInput(
            day: "2026-03-29", itemID: "0-s", title: "skipped", docTitle: "To-do",
            deadline: "2026-03-29", dueTime: "01:30", timeZone: dublin
        ))
        #expect(input.kind == .timed(try at("2026-03-29T01:30:00Z")))
        let plan = DueAlertPlanner.plan([input], timeZone: dublin, now: try at("2026-03-28T12:00:00Z"))
        #expect(plan.first?.body == "Due 02:30 · To-do")
    }

    /// The time model's instants and all-day dates go straight through.
    @Test func fromDeadline() throws {
        let timed = try #require(DueAlertInput(day: "d", itemID: "i", title: "t", docTitle: "D", deadline: .at(try at("2026-10-25T00:30:00Z"))))
        #expect(timed.kind == .timed(try at("2026-10-25T00:30:00Z")))
        let allDay = try #require(DueAlertInput(day: "d", itemID: "i", title: "t", docTitle: "D", deadline: .allDay("2026-10-25")))
        #expect(allDay.kind == .allDay(DateComponents(year: 2026, month: 10, day: 25)))
        #expect(DueAlertInput(day: "d", itemID: "i", title: "t", docTitle: "D", deadline: nil) == nil)
    }

    /// Clocks go back in Dublin at 02:00 IST on 2026-10-25 (01:00 UTC).
    @Test func dstChangeInDublin() throws {
        let now = try at("2026-10-20T12:00:00Z")
        let plan = DueAlertPlanner.plan([
            allDay("a", 2026, 10, 24), allDay("b", 2026, 10, 25), allDay("c", 2026, 10, 26),
            try timed("d", "2026-10-25T00:30:00Z"), try timed("e", "2026-10-25T01:30:00Z"),
        ], timeZone: dublin, now: now)
        let byID = Dictionary(uniqueKeysWithValues: plan.map { ($0.itemID, $0) })
        #expect(byID["a"]?.fireDate == (try at("2026-10-24T08:00:00Z")))
        #expect(byID["b"]?.fireDate == (try at("2026-10-25T09:00:00Z")))
        #expect(byID["c"]?.fireDate == (try at("2026-10-26T09:00:00Z")))
        // the repeated hour: both read 01:30 locally, an hour apart
        #expect(byID["d"]?.body == "Due 01:30 · To-do")
        #expect(byID["e"]?.body == "Due 01:30 · To-do")
        #expect(plan.map(\.itemID) == ["a", "d", "e", "b", "c"])
    }

    @Test func timeZoneChangeBetweenRuns() throws {
        let now = try at("2026-10-01T12:00:00Z")
        let inputs = [allDay("a", 2026, 10, 10), try timed("t", "2026-10-10T14:00:00Z")]
        let home = DueAlertPlanner.plan(inputs, timeZone: dublin, now: now)
        let away = DueAlertPlanner.plan(inputs, timeZone: newYork, now: now)
        // all-day: still 09:00 local, now New York's
        #expect(away.first { $0.itemID == "a" }?.fireDate == (try at("2026-10-10T13:00:00Z")))
        // timed: the same instant, shown in local time
        let t = try #require(away.first { $0.itemID == "t" })
        #expect(t.fireDate == (try at("2026-10-10T14:00:00Z")))
        #expect(t.body == "Due 10:00 · To-do")
        // the floating all-day request stays; the timed one is replaced
        let d = DueAlertPlanner.diff(pending: home, desired: away)
        #expect(d.remove == ["todo:2026-10-01/t"])
        #expect(d.add.map(\.itemID) == ["t"])
    }

    @Test func skipsCompletedAndPast() throws {
        let now = try at("2026-10-10T09:30:00Z")  // 10:30 IST
        let plan = DueAlertPlanner.plan([
            try timed("done", "2026-10-11T10:00:00Z", done: true),
            allDay("doneDay", 2026, 10, 12, done: true),
            try timed("past", "2026-10-10T09:00:00Z"),
            allDay("todayPast", 2026, 10, 10),  // 09:00 IST already gone
            try timed("soon", "2026-10-10T10:00:00Z"),
        ], timeZone: dublin, now: now)
        #expect(plan.map(\.itemID) == ["soon"])
        // all-day today still alerts when planned before 09:00
        let early = DueAlertPlanner.plan([allDay("today", 2026, 10, 10)], timeZone: dublin, now: try at("2026-10-10T06:00:00Z"))
        #expect(early.count == 1)
    }

    @Test func capsAtSixtyFourSoonestFirst() throws {
        let base = try at("2026-10-01T12:00:00Z")
        let inputs = (0..<100).reversed().map { i in
            DueAlertInput(day: "2026-10-01", itemID: "\(i)", title: "t", docTitle: "To-do", kind: .timed(base.addingTimeInterval(Double(i + 1) * 3600)))
        }
        let plan = DueAlertPlanner.plan(inputs, timeZone: dublin, now: base)
        #expect(plan.count == DueAlertPlanner.cap)
        #expect(plan.map(\.itemID) == (0..<64).map(String.init))
    }

    @Test func adaptsBothToDoModels() throws {
        // coming model: `due_at` is the instant
        let utc = try #require(DueAlertInput(day: "2026-10-01", itemID: "0-a", title: "x", docTitle: "To-do", dueAt: "2026-10-10T14:00:00Z", deadline: "2026-10-10", timeZone: newYork))
        #expect(utc.kind == .timed(try at("2026-10-10T14:00:00Z")))
        let frac = try #require(DueAlertInput(day: "2026-10-01", itemID: "0-a", title: "x", docTitle: "To-do", dueAt: "2026-10-10T14:00:00.250Z"))
        #expect(frac.kind == .timed(try at("2026-10-10T14:00:00Z").addingTimeInterval(0.25)))
        // all-day keeps a plain date
        let day = try #require(DueAlertInput(day: "2026-10-01", itemID: "0-a", title: "x", docTitle: "To-do", deadline: "2026-10-10"))
        #expect(day.kind == .allDay(DateComponents(year: 2026, month: 10, day: 10)))
        // pre-UTC answers: deadline + due_time is a wall time with no zone,
        // read as UTC as the server reads it (Deadline's legacy rule)
        let item = TodoItem(id: "2-ff", text: "Ship", done: false, deadline: "2026-10-10", dueTime: "15:00")
        let fromItem = try #require(DueAlertInput(item, day: "2026-10-09", timeZone: dublin))
        #expect(fromItem.kind == .timed(try at("2026-10-10T15:00:00Z")))
        #expect(fromItem.id == "2026-10-09/2-ff" && fromItem.docTitle == "To-do")
        #expect(DueAlertInput(TodoItem(id: "0", text: "no deadline", done: false), day: "2026-10-09") == nil)
        // cached To-do doc rows: combined legacy deadline (UTC), server item id, marks
        let rec = TodoRecord(date: "2026-10-09", position: 0, mark: " ", text: "a", deadline: "2026-10-10 15:00")
        let fromRec = try #require(DueAlertInput(rec, timeZone: dublin))
        #expect(fromRec.itemID == "0-e40c292c")  // FNV-1a("a")
        #expect(fromRec.kind == .timed(try at("2026-10-10T15:00:00Z")) && !fromRec.done)
        #expect(DueAlertInput(TodoRecord(date: "2026-10-09", position: 1, mark: ">", text: "a", deadline: "2026-10-10"))?.done == true)
        #expect(DueAlertInput.serverItemID(position: 3, text: " a ") == "3-e40c292c")
    }

    @Test func overridesHoldUntilTheInputsCatchUp() throws {
        let now = try at("2026-10-01T12:00:00Z")
        let snooze = try at("2026-10-01T13:00:00Z")
        let a = try timed("a", "2026-10-01T12:30:00Z")
        let b = try timed("b", "2026-10-01T12:30:00Z")
        let overrides: [String: DueAlertOverride] = [a.id: .done, b.id: .snoozed(snooze), "2026-10-01/gone": .done]
        let r = DueAlertPlanner.applying(overrides, to: [a, b], now: now)
        #expect(r.inputs.map(\.itemID) == ["b"])
        #expect(r.inputs.first?.kind == .timed(snooze))
        #expect(r.overrides == [a.id: .done, b.id: .snoozed(snooze)])
        // the sync shows both writes: overrides drop
        var a2 = a; a2.done = true
        var b2 = b; b2.kind = .timed(snooze)
        let caught = DueAlertPlanner.applying(r.overrides, to: [a2, b2], now: now)
        #expect(caught.overrides.isEmpty && caught.inputs == [a2, b2])
    }

    @Test func diffRemovesStaleAddsNewAndReplacesChanged() throws {
        let now = try at("2026-10-01T12:00:00Z")
        let old = DueAlertPlanner.plan([try timed("keep", "2026-10-02T10:00:00Z"), try timed("stale", "2026-10-02T10:00:00Z"), try timed("moved", "2026-10-02T10:00:00Z")], timeZone: dublin, now: now)
        let new = DueAlertPlanner.plan([try timed("keep", "2026-10-02T10:00:00Z"), try timed("moved", "2026-10-02T11:00:00Z"), try timed("fresh", "2026-10-03T10:00:00Z")], timeZone: dublin, now: now)
        let other = PlannedAlert(identifier: "someone-else", day: "", itemID: "", title: "", body: "", fireDate: now, components: DateComponents())
        let d = DueAlertPlanner.diff(pending: old + [other], desired: new)
        #expect(d.remove == ["todo:2026-10-01/moved", "todo:2026-10-01/stale"])
        #expect(d.add.map(\.itemID) == ["moved", "fresh"])
        let same = DueAlertPlanner.diff(pending: new, desired: new)
        #expect(same.remove.isEmpty && same.add.isEmpty)
    }

    @Test func enqueuesAToggle() async throws {
        let cache = try Cache.inMemory()
        let e = try await cache.enqueueToggle(date: "2026-10-01", itemID: "0-a", done: true, key: "k1")
        #expect(e.path == "/api/todo/toggle" && e.method == "POST")
        let data = try #require(e.body)
        let body = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        #expect(body["date"] as? String == "2026-10-01" && body["item_id"] as? String == "0-a" && body["done"] as? Bool == true)
    }
}
