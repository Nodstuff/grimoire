import Foundation
import Testing
@testable import TaisceKit

/// The one-surface editor's block-id model: ids must survive typing,
/// Return, Backspace, paste, cut and undo, or saves turn into delete +
/// insert and comment anchors / ^refs break.
@Suite struct SurfaceTests {
    /// A doc on the surface with a fake text view's behaviour: typing
    /// inserts with typing attributes, every change is normalised, and
    /// edits can be undone (attributed text restored, as UIKit does).
    struct Harness {
        var s: NSMutableAttributedString
        var sel = NSRange(location: 0, length: 0)
        var baseline: [DocDiff.BaselineBlock]
        var undo: [(NSRange, NSAttributedString, NSRange)] = []
        var minted = 0

        init(_ blocks: [Block]) {
            let (bs, base) = Surface.load(DocEditor(docID: "d", baseEpoch: 1, blocks: blocks))
            s = Surface.document(bs)
            baseline = base
        }

        mutating func mint() -> BlockID {
            minted += 1
            return "m\(minted)"
        }

        var text: String { s.string }
        var blocks: [Surface.SurfaceBlock] { Surface.blocks(s) }
        var ids: [BlockID] { blocks.map(\.id) }
        var markdown: [String] { blocks.map(\.content.markdown) }

        func location(of needle: String) -> Int {
            let r = (s.string as NSString).range(of: needle)
            precondition(r.location != NSNotFound, "\(needle) not in \(s.string)")
            return r.location
        }

        mutating func normalize() {
            var n = 0
            Surface.normalize(s, selection: &sel) { n += 1; return "n\(n)-\(UUID().uuidString.prefix(4))" }
        }

        mutating func apply(_ e: Surface.Edit?) {
            guard let e else { Issue.record("no edit"); return }
            if !e.isSelectionOnly {
                let old = s.attributedSubstring(from: e.range)
                undo.append((NSRange(location: e.range.location, length: e.replacement.length), old, sel))
                e.apply(to: s)
            }
            sel = e.selection
            normalize()
        }

        /// UIKit typing: the text goes in with the typing attributes.
        mutating func type(_ text: String, typing: [NSAttributedString.Key: Any]? = nil) {
            let attrs = typing ?? Surface.typingAttributes(s, at: sel.location)
            let r = sel
            let old = s.attributedSubstring(from: r)
            s.replaceCharacters(in: r, with: NSAttributedString(string: text, attributes: attrs))
            undo.append((NSRange(location: r.location, length: (text as NSString).length), old, sel))
            sel = NSRange(location: r.location + (text as NSString).length, length: 0)
            normalize()
            if let e = Surface.shortcut(s, at: sel.location) { apply(e) }
        }

        mutating func undoLast() {
            guard let (r, old, before) = undo.popLast() else { return }
            s.replaceCharacters(in: r, with: old)
            sel = before
            normalize()
        }

        mutating func returnKey() {
            var m = 0
            apply(Surface.returnKey(s, at: sel.location) { m += 1; return "r\(m)-\(UUID().uuidString.prefix(4))" })
        }

        mutating func backspace() {
            if let e = Surface.backspace(s, at: sel.location) {
                apply(e)
            } else {
                let r = Surface.expandDeletion(NSRange(location: sel.location - 1, length: 1), in: s)
                apply(Surface.replace(s, r, with: ""))
            }
        }

        func ops() -> [BlockOp] {
            DocDiff.computeOps(baseline: baseline, entries: Surface.entries(s, known: Set(baseline.map(\.id))))
        }
    }

    static func b(_ id: String, _ content: String, parent: String? = nil, key: String, type: BlockType = .paragraph) -> Block {
        Block(id: id, docID: "d", parentID: parent, orderKey: key, blockType: type, content: content)
    }

    static let sample: [Block] = [
        b("h", "# Title", key: "i", type: .heading),
        b("p1", "Hello **bold** and [[Other Doc]] world", parent: "h", key: "i"),
        b("l", "- one\n- [ ] two\n  - nested", parent: "h", key: "m"),
        b("c", "```swift\nlet x = 1\n```", parent: "h", key: "q", type: .code),
        b("q", "> quoted", parent: "h", key: "u"),
        b("p2", "last", parent: "h", key: "x"),
    ]

    // MARK: load

    @Test func untouchedDocSavesNothing() {
        let h = Harness(Self.sample)
        #expect(h.ids == ["h", "p1", "l", "c", "q", "p2"])
        #expect(h.markdown == ["# Title", "Hello **bold** and [[Other Doc]] world", "- one\n- [ ] two\n  - nested", "```swift\nlet x = 1\n```", "> quoted", "last"])
        #expect(h.ops().isEmpty)
        #expect(h.text.hasSuffix("\n"))
        #expect(h.text.contains(Surface.attachment))
    }

    @Test func nonCanonicalSourceStaysUntouched() {
        let h = Harness([Self.b("a", "Some  __bold__ text", key: "i"), Self.b("t", "| a | b |\n|---|---|\n| 1 | 2 |", key: "r")])
        #expect(h.ops().isEmpty)
    }

    @Test func emptyDocIsOneUnsavedParagraph() {
        var h = Harness([])
        #expect(h.blocks.count == 1)
        #expect(h.ops().isEmpty)
        h.type("first")
        let ops = h.ops()
        #expect(ops.count == 1)
        if case let .insert(id, parent, _, type, content) = ops.first {
            #expect(id == h.ids[0] && parent == nil && type == .paragraph && content == "first")
        } else { Issue.record("want an insert") }
    }

    // MARK: typing

    @Test func typingIsOneReplace() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: h.location(of: "world") + 5, length: 0)
        h.type("!")
        #expect(h.ops() == [.replace(target: "p1", content: "Hello **bold** and [[Other Doc]] world!")])
    }

    @Test func typingAtTheStartOfAnEmptyLineKeepsThatLine() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("e", "", key: "r"), Self.b("z", "zed", key: "x")])
        let e = h.blocks[1]
        h.sel = NSRange(location: e.range.location, length: 0)
        // the text view's default would carry the previous line's attributes
        let wrong = h.s.attributes(at: e.range.location - 1, effectiveRange: nil)
        h.type("x", typing: wrong)
        #expect(h.ids == ["a", "e", "z"])
        #expect(h.ops() == [.replace(target: "e", content: "x")])
    }

    @Test func softBreaksRoundTrip() {
        let h = Harness([Self.b("a", "line one\nline two", key: "i")])
        #expect(h.blocks.count == 1)
        #expect(h.text.contains(Surface.softBreak))
        #expect(h.ops().isEmpty)
    }

    // MARK: Return

    @Test func returnSplitsTheFirstHalfKeepsTheID() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: h.location(of: "last") + 2, length: 0)
        h.returnKey()
        #expect(h.ids.prefix(6) == ["h", "p1", "l", "c", "q", "p2"])
        #expect(h.markdown.suffix(2) == ["la", "st"])
        let ops = h.ops()
        #expect(ops.count == 2)
        #expect(ops.contains(.replace(target: "p2", content: "la")))
        #expect(ops.contains { if case let .insert(id, "h", _, .paragraph, "st") = $0 { id == h.ids[6] } else { false } })
        // the caret is at the start of the new line
        #expect(h.sel.location == h.blocks[6].range.location)
    }

    @Test func returnAtTheStartPushesDownWithoutANewBlock() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: h.location(of: "last"), length: 0)
        h.returnKey()
        #expect(h.ids.last == "p2")
        #expect(h.blocks.count == 7)
        #expect(h.ops().isEmpty, "an empty new paragraph is not saved")
        #expect(h.sel.location == h.location(of: "last"))
    }

    @Test func returnAtTheEndThenTypingInserts() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: h.location(of: "last") + 4, length: 0)
        h.returnKey()
        #expect(h.ops().isEmpty)
        h.type("after")
        let ops = h.ops()
        #expect(ops.count == 1)
        if case let .insert(_, parent, _, _, content) = ops.first { #expect(parent == "h" && content == "after") } else { Issue.record("want an insert") }
    }

    @Test func returnInAListAddsAnItemToTheSameBlock() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: h.location(of: "one") + 3, length: 0)
        h.returnKey()
        #expect(h.ids == ["h", "p1", "l", "c", "q", "p2"])
        h.type("one and a half")
        #expect(h.ops() == [.replace(target: "l", content: "- one\n- one and a half\n- [ ] two\n  - nested")])
    }

    @Test func returnOnAnEmptyItemLeavesTheList() {
        var h = Harness([Self.b("l", "- a\n- b", key: "i")])
        h.sel = NSRange(location: h.location(of: "b") + 1, length: 0)
        h.returnKey()
        h.returnKey()
        #expect(h.blocks.count == 2)
        #expect(h.ids[0] == "l")
        #expect(h.markdown == ["- a\n- b", ""])
        h.type("para")
        #expect(h.ops().count == 1)
    }

    @Test func fenceThenReturnMakesACodeBlock() {
        var h = Harness([Self.b("a", "x", key: "i")])
        h.sel = NSRange(location: 1, length: 0)
        h.returnKey()
        h.type("```")
        h.returnKey()
        #expect(h.blocks.last?.content.isRaw == true)
        #expect(h.markdown.last == "```\n\n```")
    }

    // MARK: Backspace

    @Test func backspaceAtTheStartMergesIntoTheBlockAbove() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "r")])
        h.sel = NSRange(location: h.location(of: "beta"), length: 0)
        h.backspace()
        #expect(h.ids == ["a"])
        #expect(h.markdown == ["alphabeta"])
        #expect(h.sel.location == 5)
        #expect(h.ops() == [.delete(target: "b"), .replace(target: "a", content: "alphabeta")])
    }

    @Test func backspaceAfterAnEmptyParagraphKeepsThisBlock() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("e", "", key: "m"), Self.b("b", "beta", key: "r")])
        h.sel = NSRange(location: h.location(of: "beta"), length: 0)
        h.backspace()
        #expect(h.ids == ["a", "b"])
        #expect(h.markdown == ["alpha", "beta"])
        #expect(h.ops() == [.delete(target: "e")])
    }

    @Test func backspaceOnAHeadingMakesItText() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: 0, length: 0)
        h.backspace()
        #expect(h.ids.first == "h")
        #expect(h.markdown.first == "Title")
    }

    @Test func backspaceOnASecondItemSplitsTheList() {
        var h = Harness([Self.b("l", "- a\n- b\n- c", key: "i")])
        h.sel = NSRange(location: h.location(of: "b"), length: 0)
        h.backspace()
        #expect(h.markdown == ["- a", "b", "- c"])
        #expect(h.ids[0] == "l")
    }

    @Test func backspaceInsideTextDeletesACharacter() {
        let h = Harness([Self.b("a", "alpha", key: "i")])
        #expect(Surface.backspace(h.s, at: 3) == nil)
    }

    // MARK: shortcuts

    @Test func markdownShortcutsKeepTheBlock() {
        var h = Harness([])
        let id = h.ids[0]
        h.type("#")
        h.type(" ")
        #expect(h.blocks[0].content == .heading(level: 1, AttributedString()))
        h.type("Title")
        #expect(h.ids == [id])
        #expect(h.markdown == ["# Title"])
        h.returnKey()
        h.type("-")
        h.type(" ")
        h.type("item")
        #expect(h.markdown == ["# Title", "- item"])
        #expect(h.ids[0] == id)
    }

    // MARK: deletion and paste across lines

    @Test func deletingAcrossLinesKeepsTheFirst() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "m"), Self.b("c", "gamma", key: "r")])
        let from = h.location(of: "pha")
        let to = h.location(of: "mma")
        h.apply(Surface.replace(h.s, NSRange(location: from, length: to - from), with: ""))
        #expect(h.ids == ["a"])
        #expect(h.markdown == ["almma"])
        #expect(Set(h.ops()) == [.delete(target: "b"), .delete(target: "c"), .replace(target: "a", content: "almma")])
    }

    @Test func cuttingAWholeLineJoinsTheNextIntoIt() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "m"), Self.b("c", "gamma", key: "r")])
        let from = h.location(of: "beta")
        h.apply(Surface.replace(h.s, NSRange(location: from, length: 5), with: ""))
        #expect(h.markdown == ["alpha", "gamma"])
        #expect(h.ids == ["a", "b"])
    }

    @Test func deletingTheLineBreakAtALineEndJoins() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "m")])
        h.apply(Surface.replace(h.s, NSRange(location: 5, length: 1), with: ""))
        #expect(h.ids == ["a"])
        #expect(h.markdown == ["alphabeta"])
    }

    @Test func selectAllAndDeleteLeavesOneLine() {
        var h = Harness(Self.sample)
        h.apply(Surface.replace(h.s, NSRange(location: 0, length: h.s.length - 1), with: ""))
        #expect(h.blocks.count == 1)
        #expect(h.ids == ["h"])
        #expect(h.text == "\n")
    }

    @Test func pastingLinesMidParagraph() {
        var h = Harness([Self.b("a", "start end", key: "i")])
        h.sel = NSRange(location: 6, length: 0)
        h.apply(Surface.replace(h.s, h.sel, with: "one\ntwo\nthree "))
        #expect(h.markdown == ["start one", "two", "three end"])
        #expect(h.ids[0] == "a")
        #expect(Set(h.ids).count == 3)
        #expect(h.sel.location == h.location(of: "end"))
        let ops = h.ops()
        #expect(ops.filter { if case .insert = $0 { true } else { false } }.count == 2)
    }

    @Test func pastingLinesIntoAListAddsItems() {
        var h = Harness([Self.b("l", "- a\n- b", key: "i")])
        h.sel = NSRange(location: h.location(of: "a") + 1, length: 0)
        h.apply(Surface.replace(h.s, h.sel, with: "\nx\ny"))
        #expect(h.ids == ["l"])
        #expect(h.markdown == ["- a\n- x\n- y\n- b"])
    }

    // MARK: non-text blocks

    @Test func returnBesideANonTextBlock() {
        var h = Harness(Self.sample)
        let c = h.blocks[3]
        h.sel = NSRange(location: c.range.location, length: 0)
        h.returnKey()
        #expect(h.blocks[3].content == .paragraph(AttributedString()))
        #expect(h.ids[4] == "c")
        let c2 = h.blocks[4]
        h.sel = NSRange(location: c2.range.location + 1, length: 0)
        h.returnKey()
        #expect(h.ids[4] == "c")
        #expect(h.blocks[5].content == .paragraph(AttributedString()))
        #expect(h.sel.location == h.blocks[5].range.location)
        #expect(h.ops().isEmpty)
    }

    @Test func backspaceAfterANonTextBlockSelectsItThenDeletesIt() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: h.blocks[4].range.location, length: 0)
        h.backspace() // a quote turns into text first
        h.backspace()
        let c = h.blocks[3]
        #expect(h.sel == NSRange(location: c.range.location, length: 1), "the block is selected")
        // the text view deletes the selection: the whole block goes
        let r = Surface.expandDeletion(h.sel, in: h.s)
        h.apply(Surface.replace(h.s, r, with: ""))
        #expect(!h.ids.contains("c"))
        #expect(h.ids.contains("q"))
        #expect(h.ops().contains(.delete(target: "c")))
    }

    @Test func textNeverJoinsANonTextLine() {
        var h = Harness(Self.sample)
        // delete the line break between the code block and the quote
        let c = h.blocks[3]
        h.apply(Surface.replace(h.s, NSRange(location: c.range.location + 1, length: 1), with: ""))
        #expect(h.ids.contains("c"))
        #expect(h.blocks.first { $0.id == "c" }?.content.isRaw == true)
        #expect(h.markdown.contains("> quoted") || h.markdown.contains("quoted"))
        #expect(!h.ops().contains(.delete(target: "c")))
    }

    @Test func editingANonTextBlocksSource() {
        var h = Harness(Self.sample)
        h.sel = NSRange(location: h.location(of: "last"), length: 0)
        let before = h.sel
        h.apply(Surface.replaceBlock(h.s, id: "c", with: .raw("```swift\nlet x = 2\n```"), selection: h.sel))
        #expect(h.sel == before)
        #expect(h.ops() == [.replace(target: "c", content: "```swift\nlet x = 2\n```")])
        // a source that is plain text becomes text lines
        h.apply(Surface.replaceBlock(h.s, id: "c", with: EditorBlockContent.parse(markdown: "now **text**"), selection: h.sel))
        #expect(h.blocks.first { $0.id == "c" }?.content.isRaw == false)
        #expect(h.ops() == [.replace(target: "c", content: "now **text**")])
    }

    // MARK: undo

    @Test func undoRestoresAMergedBlocksID() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "r")])
        h.sel = NSRange(location: h.location(of: "beta"), length: 0)
        h.backspace()
        #expect(h.ids == ["a"])
        h.undoLast()
        #expect(h.ids == ["a", "b"])
        #expect(h.ops().isEmpty)
    }

    @Test func undoRestoresASplit() {
        var h = Harness([Self.b("a", "alpha", key: "i")])
        h.sel = NSRange(location: 2, length: 0)
        h.returnKey()
        h.type("X")
        #expect(h.blocks.count == 2)
        h.undoLast()
        h.undoLast()
        #expect(h.ids == ["a"])
        #expect(h.ops().isEmpty)
    }

    @Test func undoRestoresACrossLineDeletion() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "m"), Self.b("c", "gamma", key: "r")])
        h.apply(Surface.replace(h.s, NSRange(location: 2, length: 10), with: "Z"))
        #expect(h.ids == ["a"])
        h.undoLast()
        #expect(h.ids == ["a", "b", "c"])
        #expect(h.ops().isEmpty)
    }

    // MARK: normalize

    @Test func duplicateIDsGetFreshOnes() {
        let s = NSMutableAttributedString()
        s.append(Surface.render(.paragraph(AttributedString("one")), id: "x"))
        s.append(Surface.render(.paragraph(AttributedString("two")), id: "x"))
        s.append(Surface.render(.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(.bullet, AttributedString("b"))]), id: "l"))
        var sel = NSRange(location: 0, length: 0)
        Surface.normalize(s, selection: &sel)
        let ids = Surface.blocks(s).map(\.id)
        #expect(ids.count == 3)
        #expect(ids[0] == "x" && ids[1] != "x" && ids[2] == "l")
    }

    @Test func orderedListsRenumberAfterADeletion() {
        var h = Harness([Self.b("l", "1. a\n2. b\n3. c", key: "i")])
        let from = h.location(of: "a") + 1
        let to = h.location(of: "b") + 1
        h.apply(Surface.replace(h.s, NSRange(location: from, length: to - from), with: ""))
        #expect(h.markdown == ["1. a\n2. c"])
        #expect(h.text.contains("2.\tc"))
        #expect(!h.text.contains("3."))
    }

    @Test func aMissingTerminatorIsAdded() {
        let s = NSMutableAttributedString(string: "abc", attributes: [Surface.block: "a", Surface.line: "p"])
        var sel = NSRange(location: 3, length: 0)
        Surface.normalize(s, selection: &sel)
        #expect(s.string == "abc\n")
        #expect(Surface.blocks(s).map(\.id) == ["a"])
        #expect(sel.location == 3)
    }

    @Test func aDeletedNonTextBlockLeavesNewText() {
        var h = Harness(Self.sample)
        let c = h.blocks[3]
        // only the U+FFFC goes (something bypassed expandDeletion)
        h.s.replaceCharacters(in: NSRange(location: c.range.location, length: 1), with: "")
        h.normalize()
        #expect(!h.ids.contains("c"))
        #expect(h.ops().contains(.delete(target: "c")))
    }

    // MARK: lists and checkboxes

    @Test func tickingABoxRewritesOnlyThatBlock() {
        var h = Harness(Self.sample)
        let ls = Surface.lines(h.s)
        let li = ls.firstIndex { $0.style.prefix?.checkbox != nil }!
        h.apply(Surface.setChecked(h.s, line: li, true, selection: h.sel))
        #expect(h.ops() == [.replace(target: "l", content: "- one\n- [x] two\n  - nested")])
        #expect(h.text.contains("☑"))
    }

    @Test func kindChangesKeepTheBlock() {
        var h = Harness([Self.b("a", "alpha", key: "i")])
        h.sel = NSRange(location: 2, length: 0)
        h.apply(Surface.transform(h.s, at: 2) { c, caret in
            EditorCommands.convert(c, to: .heading2, caret: caret).map { .update($0.0, caret: $0.1) } ?? .none
        })
        #expect(h.markdown == ["## alpha"])
        #expect(h.ids == ["a"])
        #expect(h.sel.location == 2)
    }

    @Test func caretsMapIntoListItems() {
        let h = Harness([Self.b("l", "- one\n- two", key: "i")])
        let b = h.blocks[0]
        let ls = Surface.lines(h.s)
        let c = Surface.caret(at: h.location(of: "two") + 1, in: b, lines: ls)
        #expect(c == EditorCaret(line: 1, offset: 1))
        #expect(Surface.location(of: c, in: b.content) == h.location(of: "two") + 1)
    }

    // MARK: wikilinks

    @Test func wikilinksAreAtomic() {
        let h = Harness(Self.sample)
        let at = h.location(of: "Other Doc")
        #expect(Surface.expandToWikiLinks(NSRange(location: at + 2, length: 0), in: h.s) == NSRange(location: at, length: 9))
        #expect(Surface.expandToWikiLinks(NSRange(location: at, length: 0), in: h.s) == nil)
    }

    @Test func typingAttributesFollowTheLine() {
        let h = Harness(Self.sample)
        let a = Surface.typingAttributes(h.s, at: h.location(of: "bold") + 2)
        #expect(a[Surface.block] as? String == "p1")
        #expect(a[Surface.marks] as? Int == InlineMarks.bold.rawValue)
        let start = Surface.typingAttributes(h.s, at: h.location(of: "last"))
        #expect(start[Surface.block] as? String == "p2")
        #expect(start[Surface.marks] == nil)
    }
}
