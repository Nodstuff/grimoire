import Foundation
import SwiftUI
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// The docs redesign's pure parts: change cards, the byline, history
/// groups, the outline, the Library's filter and title line, short times.
@MainActor @Suite struct DocsRedesignTests {
    static let utc: Calendar = {
        var c = Calendar(identifier: .gregorian)
        c.timeZone = TimeZone(identifier: "UTC")!
        c.locale = Locale(identifier: "en_GB")
        return c
    }()

    static func item(_ kind: DocReviewItem.Kind, _ op: String, prior: String? = nil, content: String? = nil, current: String? = nil,
                     proposer: String = "keeper:API") -> DocReviewItem {
        DocReviewItem(id: "a", docID: "d", opID: "o", kind: kind, opType: op, block: "b", content: content, prior: prior, current: current, proposer: proposer)
    }

    @Test func flaggedReplaceKeepsOrReverts() {
        let c = ChangeCardModel(Self.item(.flagged, "replace", prior: "Agents are yellow.", content: "Edits land **yellow**."))
        #expect(c.verdict == .yellow)
        #expect(c.acceptTitle == "Keep" && c.rejectTitle == "Revert")
        #expect(c.old == "Agents are yellow." && c.new == "Edits land yellow.", "markdown marks dropped")
        #expect(c.note == nil)
    }

    @Test func parkedReplaceAppliesOrDiscardsFromTheLiveText() {
        let c = ChangeCardModel(Self.item(.parked, "replace", prior: "older", content: "new", current: "live"))
        #expect(c.verdict == .red)
        #expect(c.acceptTitle == "Apply" && c.rejectTitle == "Discard")
        #expect(c.old == "live", "a parked op would change the text as it is now")
        #expect(c.new == "new")
    }

    @Test func parkedDeleteAsksTwice() {
        let item = Self.item(.parked, "delete", prior: "## Federation (cut)")
        let c = ChangeCardModel(item)
        #expect(c.acceptTitle == "Apply delete" && c.rejectTitle == "Dismiss")
        #expect(c.old == "Federation (cut)" && c.new.isEmpty)
        #expect(c.note?.contains("waits for you") == true)
        #expect(item.summary == "Delete \u{201C}Federation (cut)\u{201D}")
    }

    @Test func plainWordsFromMarkdown() {
        #expect(ReviewText.plain("## Write path") == "Write path")
        #expect(ReviewText.plain("See [[Review Gate]] and [[Agent Guide|the guide]].") == "See Review Gate and the guide.")
        #expect(ReviewText.plain("A [link](https://x.y) and `code` and **bold**") == "A link and code and bold")
    }

    @Test func shortTimesReadLikeTheArtboard() {
        let cal = Self.utc
        // Saturday 24 October 2026, 13:53:20 UTC
        let now = Date(timeIntervalSince1970: 1_792_850_000)
        let day: TimeInterval = 86_400
        #expect(ShortWhen.string(now.addingTimeInterval(-3600), now: now, calendar: cal) == "12:53")
        #expect(ShortWhen.string(now.addingTimeInterval(-day), now: now, calendar: cal) == "Yest.")
        #expect(ShortWhen.string(now.addingTimeInterval(-3 * day), now: now, calendar: cal) == "Wed")
        #expect(ShortWhen.string(now.addingTimeInterval(-5 * day), now: now, calendar: cal) == "Mon")
        #expect(ShortWhen.string(now.addingTimeInterval(-10 * day), now: now, calendar: cal) == "14 Oct")
        let old = ShortWhen.string(now.addingTimeInterval(-400 * day), now: now, calendar: cal)
        #expect(old == "19 Sept 2025" || old == "19 Sep 2025")
    }

    static func row(_ n: Int, _ who: String, kind: String = "agent", applied: Bool = true, yours: Bool? = nil, op: String = "replace",
                    content: String? = nil, at seconds: Double) -> DocHistoryEntry {
        // a v7 id whose first 48 bits are the time
        let hex = String(UInt64(seconds * 1000), radix: 16)
        let pad = String(repeating: "0", count: max(0, 12 - hex.count)) + hex
        let uuid = "\(pad.prefix(8))-\(pad.suffix(4))-7\(String(format: "%03x", n))-8000-000000000000"
        return DocHistoryEntry(opID: uuid, principalName: who, principalKind: kind, applied: applied, opType: op, content: content, principalIsYours: yours)
    }

    @Test func bylineNamesYouAndTheAgents() {
        let t: Double = 1_792_850_000
        let history = [
            Self.row(1, "keeper:API", at: t),
            Self.row(2, "tom", kind: "human", yours: true, at: t - 60),
            Self.row(3, "keeper:API", at: t - 120),
            Self.row(4, "claude:docs", at: t - 180),
            Self.row(5, "aoife", kind: "human", at: t - 240),
        ]
        let b = DocByline.make(history: history, me: "tom", version: 14)
        #expect(b.authors == ["keeper:API", "You", "claude:docs"], "newest first, three at most, no repeats")
        #expect(b.who == "keeper:API, You and claude:docs")
        #expect(b.version == 14)
        #expect(b.edited == Date(timeIntervalSince1970: t))
        #expect(DocByline(authors: ["You", "keeper:API"]).who == "You and keeper:API")
    }

    @Test func historyGroupsARunOfEdits() {
        let t: Double = 1_792_850_000
        let groups = HistoryGroup.make([
            Self.row(1, "tom", kind: "human", yours: true, at: t),
            Self.row(2, "tom", kind: "human", yours: true, at: t - 60),
            Self.row(3, "tom", kind: "human", yours: true, at: t - 120),
            Self.row(4, "keeper:API", op: "insert", content: "## MCP tools", at: t - 900),
            Self.row(5, "keeper:API", applied: false, at: t - 1000),
        ])
        #expect(groups.map(\.who) == ["You", "keeper:API", "keeper:API"])
        #expect(groups[0].count == 3 && groups[0].summary == "3 edits" && !groups[0].isAgent)
        #expect(groups[1].summary == "Added \u{201C}MCP tools\u{201D}" && groups[1].isAgent)
        #expect(!groups[2].applied, "a parked op is its own line")
    }

    @Test func outlineIsTheHeadings() {
        let o = OutlineEntry.make([("a", "Intro text"), ("b", "## Write path"), ("c", "### Gate"), ("d", "#nospace"), ("e", "# [[Top]]")])
        #expect(o.map(\.title) == ["Write path", "Gate", "Top"])
        #expect(o.map(\.level) == [2, 3, 1])
    }

    @Test func libraryFilterFindsTitlesWithWhereTheyLive() {
        let nodes = LibraryNode.build(PreviewData.docs)
        let hits = LibraryFilter.matches("sync", in: nodes)
        #expect(hits.map(\.node.doc.title) == ["Sync contract"])
        #expect(hits.first?.path == "Grimoire › iOS app")
        #expect(LibraryFilter.matches("design spec", in: nodes).map(\.id) == ["g-ios-design"], "every word")
        #expect(LibraryFilter.matches("  ", in: nodes).isEmpty)
    }

    @Test func libraryTitleLineSaysWhoSharesIt() {
        let own = Workspace(id: "w1", name: "Work", docCount: 38)
        let shared = Workspace(id: "w2", name: "Family", docCount: 1, role: "viewer", ownerName: "Aoife", shared: true)
        let picker = WorkspacePicker(workspaces: [own, shared], unsortedCount: 4, stored: .id("w1"))
        #expect(LibraryHeaderModel.subtitle(.id("w1"), picker: picker) == "38 docs · only you")
        #expect(LibraryHeaderModel.subtitle(.id("w2"), picker: picker) == "1 doc · shared by Aoife · view only")
        #expect(LibraryHeaderModel.subtitle(.unsorted, picker: picker) == "4 docs · not in a workspace")
    }

    @Test func frontmatterStatusShowsAsAPill() {
        #expect(DocPage.frontmatterStatus("---\nstatus: Decided\ntags: [a]\n---") == "decided")
        #expect(DocPage.frontmatterStatus("---\ntags: [a]\n---") == nil)
        let page = DocPage.build(title: "A", blocks: [Block(id: "f", docID: "d", parentID: nil, orderKey: "a", blockType: .code, content: "---\nstatus: draft\n---")])
        #expect(page.status == "draft")
    }

    @Test func wordDiffDrawsASpaceWhereARemovalMeetsAnAddition() {
        // the raw diff keeps both sides whole (spacing is for drawing only)
        let raw = WordDiff.segments(old: "a.", new: "b.")
        #expect(raw.filter { $0.kind != .added }.map(\.text).joined() == "a.")
        #expect(raw.filter { $0.kind != .removed }.map(\.text).joined() == "b.")
        #expect(WordDiff.display(old: "a.", new: "b.").first?.text == "a. ")
        #expect(WordDiff.display(old: "one two", new: "one three").map(\.text) == ["one ", "two ", "three"])
        let spaced = WordDiff.spaced([.init(kind: .removed, text: "two "), .init(kind: .added, text: "three")])
        #expect(spaced.map(\.text) == ["two ", "three"], "nothing added where a space already is")
    }

    @Test func formattingBarKnowsTheBlockKind() {
        #expect(FormattingBar.kind(of: .paragraph(AttributedString("x"))) == .paragraph)
        #expect(FormattingBar.kind(of: .heading(level: 2, AttributedString("x"))) == .heading2)
        let bullet = EditorListItem(ListPrefix(indent: 0, ordered: false, marker: "-", number: 1, checkbox: nil))
        let todo = EditorListItem(ListPrefix(indent: 0, ordered: false, marker: "-", number: 1, checkbox: false))
        #expect(FormattingBar.kind(of: .list([bullet])) == .bullet)
        #expect(FormattingBar.kind(of: .list([todo])) == .todo)
        #expect(FormattingBar.kind(of: .raw("```\n```")) == nil)
    }

    @Test func findInDocAsksTheDocInFront() {
        let router = Router()
        let picker = WorkspacePicker(workspaces: [], unsortedCount: 0, stored: nil, enabled: false)
        #expect(!router.canPerform(.findInDoc, signedIn: true, picker: picker), "no doc in front")
        router.tab = .library
        router.libraryPath = [.doc("d1")]
        #expect(router.canPerform(.findInDoc, signedIn: true, picker: picker))
        #expect(router.perform(.findInDoc, picker: picker) == .none)
        #expect(router.docAction?.kind == .find && router.docAction?.doc == "d1")
    }

    @Test func reviewCountsFollowADocsDecisions() {
        let counts = ReviewCounts()
        counts.set("d", 2)
        #expect(counts.count("d") == 2)
        counts.set("d", 0)
        #expect(counts.counts["d"] == nil)
    }
}

/// Inline change cards on the live surface: a covered block waits for the
/// decision; the rest of the doc edits as before.
@MainActor @Suite(.serialized) struct ChangeCardSurfaceTests {
    func harness() throws -> EditorTextTests.Harness {
        let blocks = [
            Block(id: "b0", docID: "d", parentID: nil, orderKey: "a", blockType: .paragraph, content: "first"),
            Block(id: "b1", docID: "d", parentID: nil, orderKey: "b", blockType: .paragraph, content: "flagged words"),
            Block(id: "b2", docID: "d", parentID: nil, orderKey: "c", blockType: .paragraph, content: "last"),
        ]
        let h = try EditorTextTests.Harness(blocks)
        h.tv.frame = CGRect(x: 0, y: 0, width: 390, height: 800)
        return h
    }

    static let flagged = DocReviewItem(id: "a1", docID: "d", opID: "o1", kind: .flagged, opType: "replace", block: "b1",
                                       content: "flagged words", prior: "old words", proposer: "keeper:API")

    @Test func aCoveredBlockRefusesTyping() throws {
        let h = try harness()
        h.model.setReviewItems([Self.flagged])
        h.tv.layoutIfNeeded()
        #expect(h.model.cardItems["b1"] == Self.flagged)
        #expect(h.tv.cardViews["b1"] != nil, "a card covers the flagged block")
        let inside = h.location(of: "flagged") + 3
        #expect(h.model.touchesCovered(NSRange(location: inside, length: 0)))
        #expect(!h.model.textView(h.tv, shouldChangeTextIn: NSRange(location: inside, length: 0), replacementText: "x"))
        #expect(h.text.contains("flagged words"))
        #expect(h.model.notice == "Keep or revert the change first")
        // the other blocks still take typing
        h.type("!", at: h.location(of: "first") + 5)
        #expect(h.text.hasPrefix("first!"))
    }

    @Test func theCardGoesWhenTheItemIsDecided() throws {
        let h = try harness()
        h.model.setReviewItems([Self.flagged])
        h.tv.layoutIfNeeded()
        h.model.setReviewItems([])
        h.tv.layoutIfNeeded()
        #expect(h.tv.cardViews.isEmpty)
        #expect(!h.model.touchesCovered(NSRange(location: h.location(of: "flagged"), length: 1)))
    }

    @Test func movesAndMissingBlocksGetNoCard() throws {
        let h = try harness()
        var move = Self.flagged
        move.opType = "move"
        var gone = Self.flagged
        gone.id = "a2"
        gone.block = "zz"
        h.model.setReviewItems([move, gone])
        #expect(h.model.cardItems.isEmpty, "the inspector lists them")
    }

    @Test func checkboxLinesGetTheBoxFragment() throws {
        let h = try EditorTextTests.Harness([Block(id: "l", docID: "d", parentID: nil, orderKey: "a", blockType: .paragraph, content: "- [x] done\n- [ ] open")])
        let s = h.tv.textStorage
        let lines = Surface.lines(s)
        #expect(lines.count == 2)
        let first = s.attributedSubstring(from: lines[0].range)
        let second = s.attributedSubstring(from: lines[1].range)
        #expect(SurfaceLayoutDelegate.checkbox(first) == true)
        #expect(SurfaceLayoutDelegate.checkbox(second) == false)
        #expect(SurfaceLayoutDelegate.checkbox(NSAttributedString(string: "plain")) == nil)
        // the marker glyph itself is never drawn (the fragment draws the box)
        #expect((s.attribute(.foregroundColor, at: lines[0].range.location, effectiveRange: nil) as? UIColor) == UIColor.clear)
    }
}
