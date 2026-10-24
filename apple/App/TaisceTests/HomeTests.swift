import Foundation
import Testing
import TaisceKit
@testable import Taisce

private let utc = TimeZone(identifier: "UTC") ?? .gmt
private var cal: Calendar {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = utc
    c.locale = Locale(identifier: "en_IE")
    return c
}
/// Thursday 1 October 2026, 12:00 UTC
private let noon = Date(timeIntervalSince1970: 1_790_856_000)

private func entry(_ id: String, _ due: String?, overdue: Bool = false, date: String = "2026-10-01", note: String? = nil) -> TodoEntry {
    TodoEntry(date: date, itemID: id, text: "item \(id)", due: due.flatMap(Due.init), overdue: overdue, note: note)
}

private func item(_ id: String, _ kind: ReviewQueueItem.Kind, by proposer: String = "claude:docs", summary: String = "Edit a paragraph") -> ReviewQueueItem {
    ReviewQueueItem(id: id, docID: "d\(id)", docTitle: "Doc \(id)", kind: kind, proposer: proposer, summary: summary)
}

/// Today: the review card's words, the book to continue, the due rows.
@Suite struct TodayTests {
    @Test func reviewCardTitleCountsAgentChanges() {
        #expect(ReviewCopy.title([item("1", .flagged), item("2", .flagged), item("3", .parked)]) == "3 agent changes want a look")
        #expect(ReviewCopy.title([item("1", .flagged)]) == "1 agent change wants a look")
        // a person's change in the mix: no "agent"
        #expect(ReviewCopy.title([item("1", .flagged), item("2", .parked, by: "Aoife")]) == "2 changes want a look")
    }

    @Test func reviewCardSubtitleSaysWhatDecidingMeans() {
        #expect(ReviewCopy.subtitle(ReviewQueueSummary([item("1", .flagged)])) == "Already in your docs. Keep or revert.")
        #expect(ReviewCopy.subtitle(ReviewQueueSummary([item("1", .parked)])) == "Not applied yet. Apply or discard.")
        #expect(ReviewCopy.subtitle(ReviewQueueSummary([item("1", .flagged), item("2", .parked), item("3", .parked)])) == "1 already in your docs, 2 waiting to apply.")
    }

    @Test func decisionsAreNamedByKindAndADocDeleteAsksFirst() {
        #expect(ReviewCopy.actions(.flagged) == ("Keep", "Revert"))
        #expect(ReviewCopy.actions(.parked) == ("Apply", "Discard"))
        #expect(ReviewCopy.verdict(.flagged) == .yellow && ReviewCopy.verdict(.parked) == .red)
        #expect(ReviewCopy.acceptIsDestructive(item("1", .parked, summary: "Move \u{201C}Garden plan\u{201D} to the Trash")))
        #expect(!ReviewCopy.acceptIsDestructive(item("1", .parked)))
        #expect(!ReviewCopy.acceptIsDestructive(item("1", .flagged, summary: "Delete a paragraph")))
        #expect(ReviewCopy.pastTense("Revert") == "Reverted" && ReviewCopy.pastTense("Apply") == "Applied")
        #expect(ReviewCopy.failure(APIError.notFound("x")) == "That change was already decided.")
    }

    @Test func continueReadingPicksTheNewestStartedUnfinishedBook() {
        let done = TodayBook(id: "a", title: "Done", author: "", percentage: 100)
        let fresh = TodayBook(id: "b", title: "Fresh", author: "", percentage: 0)
        let reading = TodayBook(id: "c", title: "Reading", author: "Ann", seriesIndex: 2.5, percentage: 63.9, device: "Kindle", updatedAt: "2026-10-01T10:00:00Z")
        #expect(ContinueReading.pick([done, fresh, reading])?.id == "c")
        #expect(ContinueReading.pick([done, fresh]) == nil)
        #expect(ContinueReading.percent(reading) == "63%")
        #expect(ContinueReading.byline(reading) == "Ann · Book 2.5")
        #expect(ContinueReading.whereAndWhen(reading, now: noon) == "Kindle · 2 h ago")
        #expect(ContinueReading.whereAndWhen(TodayBook(id: "d", title: "", author: "", percentage: 5)) == nil)
    }

    @Test func dueRowsAreOverdueTodayAndTheWeekAhead() {
        let board = TodoBoard.build(dated: [
            entry("late", "2026-09-30", overdue: true),
            entry("now", "2026-10-01 15:00"),
            entry("mon", "2026-10-05"),
            entry("far", "2026-10-20"),
        ], undated: [entry("plain", nil)], now: noon, timeZone: utc)
        #expect(TodayDue.rows(board, now: noon, calendar: cal).map(\.itemID) == ["late", "now", "mon"])
    }

    @Test func todosSummaryLine() {
        let board = TodoBoard.build(dated: [entry("a", "2026-09-30", overdue: true), entry("b", "2026-10-03")], undated: [entry("c", nil)], now: noon, timeZone: utc)
        #expect(TodosSummary.line(board) == "3 open · 1 overdue · 1 today")
        #expect(TodosSummary.line(TodoBoard()) == "0 open")
    }

    @Test func shortPillsDropWhatTheGroupSays() {
        #expect(DueLabel.short(entry("a", "2026-10-01 15:00"), now: noon, calendar: cal) == "15:00")
        #expect(DueLabel.short(entry("a", "2026-10-01"), now: noon, calendar: cal) == "Today")
        #expect(DueLabel.short(entry("a", "2026-10-02"), now: noon, calendar: cal) == "Tomorrow")
        #expect(DueLabel.short(entry("a", "2026-09-30", overdue: true), now: noon, calendar: cal) == "Yesterday")
        #expect(DueLabel.short(entry("a", "2026-10-05"), now: noon, calendar: cal) == "Mon")
        #expect(DueLabel.short(entry("a", "2026-11-03"), now: noon, calendar: cal) == DueLabel.dayMonth(Due(year: 2026, month: 11, day: 3), calendar: cal))
        #expect(DueLabel.short(entry("a", nil), now: noon, calendar: cal) == nil)
    }

    @Test func sourceLineIsTheNoteOrTheDayItWasWritten() {
        #expect(entry("a", nil, note: "Screens first").sourceLine(now: noon, calendar: cal) == "Screens first")
        #expect(entry("a", nil).sourceLine(now: noon, calendar: cal) == nil)
        #expect(entry("a", nil, date: "2026-09-29").sourceLine(now: noon, calendar: cal) == "From \(DueLabel.shortDate(Due(year: 2026, month: 9, day: 29), calendar: cal))")
    }

    @Test func composerFormatsTheParsedDeadline() {
        // the day reads in the device's locale ("Fri 30 Oct" / "Fri Oct 30"); the time is 24-hour
        let day = DueLabel.shortDate(Due(year: 2026, month: 10, day: 30), calendar: cal)
        #expect(TodoComposer.format(Due(year: 2026, month: 10, day: 30, hour: 10, minute: 0), calendar: cal) == "\(day), 10:00")
        #expect(TodoComposer.format(Due(year: 2026, month: 10, day: 30), calendar: cal) == day)
    }

    @Test func lastUsedReadsLikeTheArtboard() {
        #expect(LastUsed.text(nil) == "Never used")
        #expect(LastUsed.text("2026-10-01T10:00:00Z", now: noon) == "Last used 2 h ago")
    }
}

/// Search's Ask: what an answer doc reads as.
@Suite struct AskAnswerTextTests {
    static let doc = """
    ---
    question: How does the gate pick a verdict?
    ---

    # How does the gate pick a verdict?

    Deletes always park; an edit to a block that changed since its author read it lands **yellow** ([[Review Gate#^abc123]], [[Architecture#^def|the architecture]]).

    ## Review Gate

    > A write whose base is older than the block lands yellow.
    >
    > — [[Review Gate#^abc123]]

    ## Architecture

    > Every change is a proposal.
    >
    > — [[Architecture#^def]]

    ## Review Gate

    > Deletes are parked.

    ---

    *Asked 2026-10-24 · 3 blocks across 2 docs.*
    """

    @Test func readsTheSynthesisAndTheSources() {
        let a = AskAnswerText.parse(Self.doc)
        #expect(a.body == "Deletes always park; an edit to a block that changed since its author read it lands **yellow** (Review Gate, the architecture).")
        #expect(a.sources == ["Review Gate", "Architecture"])
        #expect(!a.pending)
    }

    @Test func thePlaceholderMeansStillWriting() {
        let md = "# Q\n\n*✨ Writing a synthesis from the excerpts below…*\n\n## Doc\n\n> x\n"
        let a = AskAnswerText.parse(md)
        #expect(a.pending && a.body == nil && a.sources == ["Doc"])
    }

    @Test func noSynthesisWithoutAModel() {
        let a = AskAnswerText.parse("# Q\n\n## Doc\n\n> the excerpt\n\n---\n\n*Asked*\n")
        #expect(a.body == nil && !a.pending && a.sources == ["Doc"])
    }

    @Test func linksReadAsTheirTitles() {
        #expect(AskAnswerText.readLinks("see [[A#^x]] and [[B|bee]] and [[C]] [[open") == "see A and bee and C [[open")
    }
}

/// The shell's routes: Review over Today on a phone, the sidebar's Review
/// and Books, Continue reading's reader over Books.
@MainActor @Suite struct ShellRouteTests {
    @Test func reviewPushesOverTodayOnAPhoneAndSelectsInTheSidebar() {
        let phone = Router()
        phone.tab = .library
        phone.showReview()
        #expect(phone.tab == .today && phone.todayPath == [.review])
        let pad = Router()
        pad.isPad = true
        pad.showReview()
        #expect(pad.padItem == .review && pad.sidebarSelection == .review)
        pad.open(.doc("d1"))
        #expect(pad.sidebarSelection == .doc("d1"))
    }

    @Test func continueReadingOpensTheReaderOverBooks() {
        let phone = Router()
        phone.readBook("b1")
        #expect(phone.tab == .books && phone.booksPath == [.bookReader("b1")])
        let pad = Router()
        pad.isPad = true
        pad.readBook("b1")
        #expect(pad.padItem == .books && pad.padPath == [.bookReader("b1")] && pad.sidebarSelection == .books)
        pad.showBooks()
        #expect(pad.padPath.isEmpty && pad.sidebarSelection == .books)
    }

    @Test func booksTabPushesItsOwnStack() {
        let r = Router()
        r.tab = .books
        r.open(.doc("d1"))
        #expect(r.booksPath == [.doc("d1")] && r.focusedDoc == "d1" && r.visibleDocs == ["d1"])
        r.drop(["d1"])
        #expect(r.booksPath.isEmpty)
    }

    @Test func searchFocusIsARequest() {
        let r = Router()
        let before = r.searchFocusRequest
        r.focusSearch()
        #expect(r.searchFocusRequest == before + 1)
    }
}
