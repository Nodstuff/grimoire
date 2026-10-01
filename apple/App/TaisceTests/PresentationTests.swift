import Foundation
import Testing
import TaisceKit
@testable import Taisce

/// UTC everywhere so day boundaries don't depend on the machine.
private let utc = TimeZone(identifier: "UTC") ?? .gmt
private var cal: Calendar {
    var c = Calendar(identifier: .gregorian)
    c.timeZone = utc
    c.locale = Locale(identifier: "en_IE")
    return c
}
/// Thursday 1 October 2026, 12:00 UTC
private let noon = Date(timeIntervalSince1970: 1_790_856_000)

private func entry(_ id: String, _ due: String?, overdue: Bool = false, text: String = "item") -> TodoEntry {
    TodoEntry(date: "2026-10-01", itemID: id, text: text, due: due.flatMap(Due.init), overdue: overdue)
}

@Suite struct TodoBoardTests {
    @Test func empty() {
        let b = TodoBoard.build(dated: [], now: noon, timeZone: utc)
        #expect(b.isEmpty && b.count == 0 && b.due.isEmpty)
    }

    @Test func oneOverdue() {
        let b = TodoBoard.build(dated: [entry("a", "2026-09-30", overdue: true)], now: noon, timeZone: utc)
        #expect(b.overdue.map(\.itemID) == ["a"])
        #expect(b.today.isEmpty && b.upcoming.isEmpty)
        #expect(b.due.map(\.itemID) == ["a"])
    }

    @Test func manySortIntoSectionsSoonestFirst() {
        let b = TodoBoard.build(
            dated: [
                entry("up2", "2026-10-09"),
                entry("today-late", "2026-10-01 18:00"),
                entry("od", "2026-09-29", overdue: true),
                entry("up1", "2026-10-02 09:00"),
                entry("today-early", "2026-10-01 13:00"),
                entry("od-today", "2026-10-01 10:00", overdue: true),
            ],
            undated: [entry("undated", nil), entry("od", "2026-09-29", overdue: true)],
            now: noon, timeZone: utc
        )
        #expect(b.overdue.map(\.itemID) == ["od", "od-today"])
        #expect(b.today.map(\.itemID) == ["today-early", "today-late", "undated"])
        #expect(b.upcoming.map(\.itemID) == ["up1", "up2"])
        // Today's DUE card: overdue first, then dated items for today (undated stays off it)
        #expect(b.due.map(\.itemID) == ["od", "od-today", "today-early", "today-late"])
        #expect(b.count == 7)
    }

    @Test func serverItemIDMatchesDaemonHash() {
        // fnv1a("a") = 0xe40c292c (FNV-1a 32 test vector)
        #expect(TodoEntry.itemID(position: 0, text: "a") == "0-e40c292c")
        #expect(TodoEntry.itemID(position: 3, text: " a ") == "3-e40c292c")
    }

    @Test func overdueFromCacheByTimeOrDay() {
        #expect(TodoEntry.isOverdue(Due("2026-10-01 11:59").unsafelyUnwrappedForTest, now: noon, timeZone: utc))
        #expect(!TodoEntry.isOverdue(Due("2026-10-01 12:30").unsafelyUnwrappedForTest, now: noon, timeZone: utc))
        #expect(!TodoEntry.isOverdue(Due("2026-10-01").unsafelyUnwrappedForTest, now: noon, timeZone: utc))
        #expect(TodoEntry.isOverdue(Due("2026-09-30").unsafelyUnwrappedForTest, now: noon, timeZone: utc))
    }

    @Test func snoozeTargets() {
        #expect(Snooze.oneHour.deadline(now: noon, calendar: cal) == Due(year: 2026, month: 10, day: 1, hour: 13, minute: 0))
        #expect(Snooze.tomorrowMorning.deadline(now: noon, calendar: cal) == Due(year: 2026, month: 10, day: 2, hour: 9, minute: 0))
    }
}

@Suite struct DueLabelTests {
    func label(_ due: String?, overdue: Bool = false) -> DueLabel {
        DueLabel.make(entry("x", due, overdue: overdue), now: noon, calendar: cal)
    }

    @Test func tonesAndText() {
        #expect(label(nil) == DueLabel(tone: .none, text: nil))
        #expect(label("2026-09-30", overdue: true) == DueLabel(tone: .overdue, text: "Overdue · yesterday"))
        #expect(label("2026-10-01 10:00", overdue: true) == DueLabel(tone: .overdue, text: "Overdue · 10:00"))
        #expect(label("2026-09-27", overdue: true) == DueLabel(tone: .overdue, text: "Overdue · 4 days ago"))
        #expect(label("2026-10-01 15:00") == DueLabel(tone: .today, text: "Today · 15:00"))
        #expect(label("2026-10-01") == DueLabel(tone: .today, text: "Today"))
        #expect(label("2026-10-02 09:00") == DueLabel(tone: .later, text: "Tomorrow · 09:00"))
        #expect(label("2026-10-03").text == "Sat")
        #expect(label("2026-10-12").text == "Mon 12 Oct")
        #expect(label("2026-10-03").tone == .later)
    }
}

@Suite struct LibraryTests {
    @Test func empty() {
        #expect(LibraryNode.build([]).isEmpty)
    }

    @Test func oneLeaf() {
        let nodes = LibraryNode.build([DocInfo(id: "a", title: "Alpha")])
        #expect(nodes.count == 1)
        #expect(nodes[0].children == nil && nodes[0].subtitle == nil && !nodes[0].isFolder)
    }

    @Test func manyNestedWithSubtitlesAndOrphans() {
        let docs = [
            DocInfo(id: "g", title: "Grimoire", sortKey: "a"),
            DocInfo(id: "i", parentID: "g", title: "iOS app", sortKey: "b"),
            DocInfo(id: "r", parentID: "g", title: "Roadmap", sortKey: "a"),
            DocInfo(id: "d", parentID: "i", title: "Design", sortKey: "a"),
            DocInfo(id: "o", parentID: "gone", title: "Orphan", sortKey: "z"),
        ]
        let nodes = LibraryNode.build(docs)
        #expect(nodes.map(\.doc.title) == ["Grimoire", "Orphan"])
        let g = nodes[0]
        #expect(g.subtitle == "2 docs")
        #expect(g.children?.map(\.doc.title) == ["Roadmap", "iOS app"])
        let design = g.children?[1].children?[0]
        #expect(design?.subtitle == "Grimoire › iOS app")
        #expect(g.children?[1].subtitle == "1 doc")
    }

    @Test func breadcrumbsAndWikilinks() {
        let index = PreviewData.index
        #expect(index.breadcrumb(of: "g-ios-design") == "Grimoire › iOS app")
        #expect(index.breadcrumb(of: "g") == nil)
        #expect(index.doc(titled: "sync contract")?.id == "g-ios-sync")
        #expect(index.doc(titled: "Grimoire/iOS app/Design spec")?.id == "g-ios-design")
        #expect(index.doc(titled: "Nope") == nil)
    }

    @Test func parentCycleStillShowsAtTheRoot() {
        let nodes = LibraryNode.build([DocInfo(id: "a", parentID: "b", title: "A"), DocInfo(id: "b", parentID: "a", title: "B")])
        #expect(nodes.count == 1)
        #expect(nodes[0].children?.count == 1)
    }

    @Test func ancestorsStopOnCycle() {
        let index = DocIndex([DocInfo(id: "a", parentID: "b", title: "A"), DocInfo(id: "b", parentID: "a", title: "B")])
        #expect(index.ancestors(of: "a").map(\.id) == ["b"])
    }
}

@Suite struct TodayTests {
    @Test func pinnedCardsKeepPinOrderAndDropMissing() {
        let meta = ["g-road": EditMeta(author: "claude", date: noon.addingTimeInterval(-7200))]
        #expect(DocCardModel.pinned([], index: PreviewData.index, meta: meta).isEmpty)
        let one = DocCardModel.pinned(["g-road"], index: PreviewData.index, meta: meta)
        #expect(one.count == 1 && one[0].subtitle(now: noon) == "Grimoire · 2 h ago")
        let many = DocCardModel.pinned(["home-garden", "gone", "g-road", "g"], index: PreviewData.index, meta: meta)
        #expect(many.map(\.id) == ["home-garden", "g-road", "g"])
        #expect(many[0].subtitle(now: noon) == "Home")
        #expect(many[2].subtitle(now: noon) == nil)
    }

    @Test func relativeTime() {
        #expect(RelativeTime.string(noon.addingTimeInterval(-20), now: noon, calendar: cal) == "just now")
        #expect(RelativeTime.string(noon.addingTimeInterval(-300), now: noon, calendar: cal) == "5 min ago")
        #expect(RelativeTime.string(noon.addingTimeInterval(-7200), now: noon, calendar: cal) == "2 h ago")
        #expect(RelativeTime.string(noon.addingTimeInterval(-86400), now: noon, calendar: cal) == "yesterday")
        #expect(RelativeTime.string(noon.addingTimeInterval(-3 * 86400), now: noon, calendar: cal) == "3 d ago")
        #expect(RelativeTime.dayLine(noon, calendar: cal) == "Thursday 1 October")
    }

    @Test func editMetaPicksNewestApplied() {
        #expect(EditMeta(history: []) == nil)
        let rows = [
            DocHistoryEntry(opID: "0199a0b0-c0d0-7abc-8def-0123456789ab", principalName: "tom", principalKind: "human", applied: false),
            DocHistoryEntry(opID: "0199a0b0-c0c0-7abc-8def-0123456789ab", principalName: "claude", principalKind: "agent"),
        ]
        #expect(EditMeta(history: rows)?.author == "claude")
    }

    @Test func syncBadge() {
        #expect(SyncBadge.make(status: .live, pending: 0) == SyncBadge(tone: .saved, text: "Saved"))
        #expect(SyncBadge.make(status: .live, pending: 2).tone == .busy)
        #expect(SyncBadge.make(status: .waiting(retryIn: .seconds(3)), pending: 2) == SyncBadge(tone: .offline, text: "Offline · 2 pending"))
        #expect(SyncBadge.make(status: .idle, pending: 0).text == "Offline")
        #expect(SyncBadge.make(status: .waiting(retryIn: .seconds(3)), pending: 0) == SyncBadge(tone: .offline, text: "Offline"))
    }

    @Test func emptyBoardIsLoadingUntilTheFirstSync() {
        let empty = TodoBoard()
        let one = TodoBoard.build(dated: [entry("a", "2026-10-01 13:00")], now: noon, timeZone: utc)
        #expect(TodoBoard.shown(nil, hasSynced: true, status: .live) == nil)
        #expect(TodoBoard.shown(empty, hasSynced: false, status: .catchingUp) == nil)
        #expect(TodoBoard.shown(empty, hasSynced: false, status: .idle) == nil)
        #expect(TodoBoard.shown(empty, hasSynced: true, status: .live) == empty)
        // offline with nothing cached: show the empty state, not a spinner forever
        #expect(TodoBoard.shown(empty, hasSynced: false, status: .waiting(retryIn: .seconds(2))) == empty)
        #expect(TodoBoard.shown(one, hasSynced: false, status: .catchingUp) == one)
    }
}

@Suite struct DocPageTests {
    func block(_ i: Int, _ content: String, type: BlockType = .paragraph) -> Block {
        Block(id: "b\(i)", docID: "d", parentID: nil, orderKey: "\(i)", blockType: type, content: content)
    }

    @Test func empty() {
        let page = DocPage.build(title: "T", blocks: [])
        #expect(page.isEmpty && page.tags.isEmpty)
    }

    @Test func oneParagraph() {
        let page = DocPage.build(title: "T", blocks: [block(0, "Hello **world**")])
        #expect(page.blocks.count == 1)
        #expect(page.blocks[0].nodes == [.paragraph(inline: "Hello **world**")])
    }

    @Test func manyWithFrontmatterTitleAndComments() {
        let page = DocPage.build(title: "Design", blocks: [
            block(0, "---\ntags:\n  - Design\n  - ios\n---", type: .code),
            block(1, "# Design"),
            block(2, "Body"),
            block(3, "a comment", type: .comment),
            block(4, "## Next"),
        ])
        #expect(page.tags == ["design", "ios"])
        #expect(page.blocks.map(\.id) == ["b2", "b4"])
    }

    @Test func inlineTagList() {
        #expect(DocPage.frontmatterTags("---\ntitle: x\ntags: [a, \"#b\"]\n---") == ["a", "b"])
        #expect(DocPage.frontmatterTags("no frontmatter") == [])
    }

    @Test func checkboxToggle() {
        let md = "- [ ] one\n- [x] two\n  - [ ] nested\n```\n- [ ] in code\n```\n1. [ ] numbered"
        #expect(Checkbox.toggled(md, index: 0, checked: true)?.hasPrefix("- [x] one") == true)
        #expect(Checkbox.toggled(md, index: 1, checked: false)?.contains("- [ ] two") == true)
        #expect(Checkbox.toggled(md, index: 2, checked: true)?.contains("  - [x] nested") == true)
        #expect(Checkbox.toggled(md, index: 3, checked: true)?.contains("1. [x] numbered") == true)
        #expect(Checkbox.toggled(md, index: 3, checked: true)?.contains("- [ ] in code") == true)
        #expect(Checkbox.toggled(md, index: 4, checked: true) == nil)
        #expect(Checkbox.toggled("plain [ ] text", index: 0, checked: true) == nil)
    }

    @Test func checkboxCountsMatchRenderOrder() {
        let nodes = BlockRenderer.render(markdown: "- [ ] one\n- [x] two\n  - [ ] nested\n- plain")
        #expect(RenderMetrics.checkboxCount(nodes) == 3)
        guard case let .list(_, _, items)? = nodes.first else { Issue.record("not a list"); return }
        #expect(RenderMetrics.itemOffsets(items) == [0, 1, 3])
    }
}

@Suite struct RenderCacheTests {
    func block(_ content: String, id: String = "b") -> Block {
        Block(id: id, docID: "d", parentID: nil, orderKey: "a", blockType: .paragraph, content: content)
    }

    @Test func reusesUnchangedBlocksAndReparsesEdits() {
        let cache = RenderCache()
        #expect(cache.count == 0)
        let first = cache.nodes(for: block("Hello"))
        #expect(cache.nodes(for: block("Hello")) == first && cache.count == 1)
        #expect(cache.nodes(for: block("Hello **you**")) == [.paragraph(inline: "Hello **you**")])
        #expect(cache.count == 2)
    }

    @Test func manyBlocksOffTheMainActorAndALimit() async {
        let cache = RenderCache(limit: 10)
        let blocks = (0..<25).map { block("para \($0)", id: "b\($0)") }
        let page = await Task.detached { DocPage.build(title: "T", blocks: blocks, render: cache.nodes(for:)) }.value
        #expect(page.blocks.count == 25)
        #expect(cache.count <= 10)
    }
}

@Suite struct SearchTests {
    let index = PreviewData.index

    @Test func empty() {
        let s = SearchState.build(hits: [], query: "x", index: index, tags: [:])
        #expect(s.results.isEmpty && s.tags.isEmpty)
    }

    @Test func oneHit() {
        let s = SearchState.build(hits: [("b", "g-ios-sync", "", "follow the **change** cursor")], query: "change", index: index, tags: [:])
        #expect(s.results.count == 1)
        #expect(s.results[0].title == "Sync contract")
        #expect(s.results[0].breadcrumb == "Grimoire › iOS app")
        #expect(s.results[0].snippet == "follow the change cursor")
    }

    @Test func manyDedupePerDocAndDeriveTags() {
        let s = PreviewData.search
        #expect(s.results.count == 3)
        #expect(s.tags == ["ios", "architecture", "design", "sync"])
        #expect(s.filtered(by: "ios").map(\.docID) == ["g-ios-design", "g-ios-sync"])
        #expect(s.filtered(by: nil).count == 3)
        let dup = SearchState.build(hits: [("1", "g", "G", "a"), ("2", "g", "G", "b")], query: "a", index: index, tags: [:])
        #expect(dup.results.map(\.blockID) == ["1"])
    }

    @Test func snippetWindowsAroundTheMatch() {
        let long = String(repeating: "lorem ipsum ", count: 30) + "needle" + String(repeating: " dolor sit", count: 30)
        let s = SearchState.snippet(long, query: "needle", radius: 20)
        #expect(s.hasPrefix("…") && s.hasSuffix("…") && s.contains("needle"))
        #expect(s.count < 60)
    }

    @Test func plainTextStripsMarkdown() {
        #expect(SearchState.plainText("## A [[Doc|label]] and [link](http://x) `code` *em*") == "A label and link code em")
        #expect(SearchState.plainText("| a | b |\n|---|---|\n| c | d |") == "a · b c · d")
    }

    @Test func matchesAreCaseInsensitive() {
        let text = "Sync and SYNC and sink"
        #expect(SearchState.matches(in: text, query: "sync").count == 2)
        #expect(SearchState.matches(in: text, query: "sync sink").count == 3)
    }
}

private extension Optional {
    /// Test-only: the fixtures above are valid literals.
    var unsafelyUnwrappedForTest: Wrapped {
        guard let self else { preconditionFailure("fixture failed to parse") }
        return self
    }
}

@MainActor @Suite struct DueAlertTests {
    @Test func promptPerStatus() {
        #expect(DueAlertPrompt(.notDetermined) == .turnOn && DueAlertPrompt(.notDetermined).showsTodosCard)
        #expect(DueAlertPrompt(.allowed) == .on && !DueAlertPrompt(.allowed).showsTodosCard)
        #expect(DueAlertPrompt(.denied) == .openSettings && !DueAlertPrompt(.denied).showsTodosCard)
        #expect(DueAlertPrompt(.denied).settingsTitle == "Allow notifications in Settings")
    }

    @Test func askingFlipsTheStatusAndHidesTheCard() async {
        let alerts: any DueAlertPermission = PreviewDueAlerts(.notDetermined)
        #expect(DueAlertPrompt(alerts.status).showsTodosCard)
        await alerts.requestAuthorization()
        #expect(alerts.status == .allowed)
        #expect(!DueAlertPrompt(alerts.status).showsTodosCard)
    }
}

@Suite struct TodoDocAddressTests {
    func blocks(_ md: [String]) -> [DocBlock] {
        md.enumerated().map { DocBlock(id: "b\($0.offset)", content: $0.element, nodes: []) }
    }

    @Test func emptyAndOutsideADay() {
        #expect(TodoDocAddress.locate([], block: "b0", index: 0) == nil)
        #expect(TodoDocAddress.locate(blocks(["- [ ] no day"]), block: "b0", index: 0) == nil)
    }

    @Test func oneItem() {
        let b = blocks(["## 2026-10-01", "- [ ] only"])
        let at = TodoDocAddress.locate(b, block: "b1", index: 0)
        #expect(at?.date == "2026-10-01" && at?.position == 0)
    }

    @Test func manyDaysCountCarriedItemsButNotAsBoxes() {
        let b = blocks([
            "## 2026-09-30", "- [>] carried\n- [x] done",
            "## 2026-10-01", "- [ ] a\n- [>] b\n- [ ] c",
        ])
        #expect(TodoDocAddress.locate(b, block: "b1", index: 0).map { [$0.date, String($0.position)] } == ["2026-09-30", "1"])
        // the second GFM box in b3 is "c": position 2 because [>] b counts
        #expect(TodoDocAddress.locate(b, block: "b3", index: 1).map { [$0.date, String($0.position)] } == ["2026-10-01", "2"])
        #expect(TodoDocAddress.locate(b, block: "b3", index: 2) == nil)
    }
}
