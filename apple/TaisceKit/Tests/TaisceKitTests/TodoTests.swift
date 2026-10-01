import Foundation
import Testing
@testable import TaisceKit

@Suite struct DueTests {
    @Test func parsesDateOnlyAndDateTime() throws {
        let d = try #require(Due("2026-09-12"))
        #expect(d == Due(year: 2026, month: 9, day: 12))
        #expect(!d.hasTime)
        let t = try #require(Due("2026-09-12 14:30"))
        #expect(t.hour == 14 && t.minute == 30)
        #expect(t.description == "2026-09-12 14:30")
    }

    @Test(arguments: ["", "2026-9-12", "2026-02-30", "2026-13-01", "2026-09-12 24:00", "2026-09-12 9:30", "2026-09-12T14:30", "due fri"])
    func rejects(_ s: String) {
        #expect(Due(s) == nil)
    }

    @Test func dateOnlyAlertsAtNine() throws {
        let utc = try #require(TimeZone(identifier: "UTC"))
        let alert = try #require(Due("2026-09-12")?.alertDate(in: utc))
        let c = Calendar(identifier: .gregorian).dateComponents(in: utc, from: alert)
        #expect(c.hour == 9 && c.minute == 0 && c.day == 12)
        let timed = try #require(Due("2026-09-12 07:15")?.alertDate(in: utc))
        #expect(Calendar(identifier: .gregorian).dateComponents(in: utc, from: timed).hour == 7)
    }

    @Test func ordersByAlertTime() throws {
        let early = try #require(Due("2026-09-12 08:00"))
        let dateOnly = try #require(Due("2026-09-12"))
        let late = try #require(Due("2026-09-12 17:00"))
        #expect([late, dateOnly, early].sorted() == [early, dateOnly, late])
    }

    @Test func decodesServerItem() throws {
        let json = #"{"id":"0-9a1f0c2e","text":"Ship","done":false,"deadline":"2026-09-12 14:30","overdue":true,"due_soon":false}"#
        let item = try JSONDecoder().decode(TodoItem.self, from: Data(json.utf8))
        #expect(item.due == Due(year: 2026, month: 9, day: 12, hour: 14, minute: 30))
        #expect(!item.carried && item.note == nil && item.overdue)
    }
}

@Suite struct TodoParserTests {
    let doc = """
    # To-do

    ## 2026-09-09

    - [>] call the bank · due 2026-09-09

    - [x] done thing

    ## 2026-09-10

    - [ ] Ship 0.8.0 · due 2026-09-12 14:30 (carried from 2026-09-09)
      needs Tom's sign-off
      after the local trial

    - [ ] old style ⏰ 2026-09-11 thing
    - [ ] a · b · due 2026-09-10
    - [ ] due fri is just text

    some prose under the day

    ## Notes

    - [ ] not a day item
    """

    @Test func parsesCanonicalForm() throws {
        let items = TodoParser.parse(markdown: doc)
        #expect(items.count == 6)
        #expect(items[0].mark == ">" && items[0].deadline == "2026-09-09")
        #expect(items[1].mark == "x" && items[1].date == "2026-09-09")

        let ship = items[2]
        #expect(ship.date == "2026-09-10" && ship.position == 0)
        #expect(ship.text == "Ship 0.8.0")
        #expect(ship.deadline == "2026-09-12 14:30")
        #expect(ship.carriedFrom == "2026-09-09")
        #expect(ship.note == "needs Tom's sign-off\nafter the local trial")

        #expect(items[3].text == "old style thing" && items[3].deadline == "2026-09-11")
        #expect(items[4].text == "a · b" && items[4].deadline == "2026-09-10")
        #expect(items[5].text == "due fri is just text" && items[5].deadline == nil)
    }

    @Test func dueOrOverdueIsOpenAndOnOrBeforeToday() throws {
        let utc = try #require(TimeZone(identifier: "UTC"))
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = utc
        let now = try #require(cal.date(from: DateComponents(year: 2026, month: 9, day: 12, hour: 8)))
        let due = TodoParser.dueOrOverdue(TodoParser.parse(markdown: doc), now: now, timeZone: utc)
        // the [>] and [x] items are excluded; later-today 14:30 still counts as due today
        #expect(due.map(\.text) == ["a · b", "old style thing", "Ship 0.8.0"])
    }
}
