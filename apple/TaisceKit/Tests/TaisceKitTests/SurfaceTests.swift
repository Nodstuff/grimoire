import Foundation
import Testing
@testable import TaisceKit

/// The one-surface editor's block-id model: ids must survive typing,
/// Return, Backspace, paste, cut and undo, or saves turn into delete +
/// insert and comment anchors / ^refs break.
@Suite struct SurfaceTests {
    /// Ids an operation may legitimately create (Return, paste, insert).
    final class Minter: @unchecked Sendable {
        var allowed: Set<BlockID> = []
        var n = 0
        func mint() -> BlockID {
            n += 1
            let id = "m\(n)"
            allowed.insert(id)
            return id
        }
    }

    /// A doc on the surface with a fake text view's behaviour: typing
    /// inserts with typing attributes, every change is normalised, and
    /// edits can be undone (attributed text restored, as UIKit does).
    /// After every step the invariant holds: ids are unique, each comes
    /// from the doc or from an edit allowed to mint one, and `normalize`
    /// mints nothing (it asserts in debug builds if it would).
    struct Harness {
        var s: NSMutableAttributedString
        var sel = NSRange(location: 0, length: 0)
        var baseline: [DocDiff.BaselineBlock]
        var undo: [(NSRange, NSAttributedString, NSRange)] = []
        let minter = Minter()
        /// skip the invariant (a test that breaks it on purpose)
        var checking = true

        init(_ blocks: [Block]) {
            let (bs, base) = Surface.load(DocEditor(docID: "d", baseEpoch: 1, blocks: blocks))
            s = Surface.document(bs, mint: minter.mint)
            baseline = base
            minter.allowed.formUnion(base.map(\.id))
            minter.allowed.formUnion(Surface.blocks(s).map(\.id))
        }

        var mint: () -> BlockID { minter.mint }
        var text: String { s.string }
        var blocks: [Surface.SurfaceBlock] { Surface.blocks(s) }
        var ids: [BlockID] { blocks.map(\.id) }
        var markdown: [String] { blocks.map(\.content.markdown) }

        func location(of needle: String) -> Int {
            let r = (s.string as NSString).range(of: needle)
            precondition(r.location != NSNotFound, "\(needle) not in \(s.string)")
            return r.location
        }

        func check(sourceLocation: SourceLocation = #_sourceLocation) {
            guard checking else { return }
            let ids = self.ids
            #expect(Set(ids).count == ids.count, "ids unique: \(ids)", sourceLocation: sourceLocation)
            let stray = Set(ids).subtracting(minter.allowed)
            #expect(stray.isEmpty, "ids no edit made: \(stray)", sourceLocation: sourceLocation)
            #expect(s.string.hasSuffix("\n"), sourceLocation: sourceLocation)
        }

        mutating func normalize() {
            Surface.normalize(s, selection: &sel) {
                Issue.record("normalize minted an id")
                return "n-\(UUID().uuidString.prefix(4))"
            }
            check()
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

        /// The surface's own replace (deletion, typing across lines, paste).
        mutating func replace(_ r: NSRange, with t: String) {
            apply(Surface.replace(s, r, with: t, mint: mint))
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
            apply(Surface.returnKey(s, at: sel.location, mint: mint))
        }

        mutating func backspace() {
            if let e = Surface.backspace(s, at: sel.location, mint: mint) {
                apply(e)
            } else {
                let r = Surface.expandDeletion(NSRange(location: sel.location - 1, length: 1), in: s)
                replace(r, with: "")
            }
        }

        /// Forward delete (fn-Delete) at the caret.
        mutating func forwardDelete() {
            let r = Surface.expandDeletion(NSRange(location: sel.location, length: 1), in: s)
            replace(r, with: "")
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
        // the heading's section goes with it: its children move up, nothing else
        let ops = h.ops()
        #expect(ops.contains(.replace(target: "h", content: "Title")))
        let moved = ops.compactMap { op -> (BlockID, BlockID?)? in if case let .move(t, p, _) = op { (t, p) } else { nil } }
        #expect(Set(moved.map(\.0)) == ["p1", "l", "c", "q", "p2"])
        #expect(moved.allSatisfy { $0.1 == nil })
        #expect(ops.count == 1 + moved.count)
    }

    @Test func backspaceOnASecondItemSplitsTheList() {
        var h = Harness([Self.b("l", "- a\n- b\n- c", key: "i")])
        h.sel = NSRange(location: h.location(of: "b"), length: 0)
        h.backspace()
        #expect(h.markdown == ["- a", "b", "- c"])
        #expect(h.ids[0] == "l")
        let ops = h.ops()
        #expect(ops.contains(.replace(target: "l", content: "- a")))
        let inserts = ops.compactMap { op -> (String, String)? in if case let .insert(_, nil, key, _, content) = op { (key, content) } else { nil } }
        #expect(inserts.map(\.1) == ["b", "- c"])
        #expect(inserts.allSatisfy { $0.0 > "i" }, "after the list")
        #expect(inserts[0].0 < inserts[1].0)
        #expect(ops.count == 3)
    }

    @Test func aSplitBeforeASiblingKeysBetweenThem() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("z", "zulu", key: "r")])
        h.sel = NSRange(location: 2, length: 0)
        h.returnKey()
        let ops = h.ops()
        #expect(ops.contains(.replace(target: "a", content: "al")))
        let keys = ops.compactMap { op -> String? in if case let .insert(_, _, key, _, "pha") = op { key } else { nil } }
        #expect(keys.count == 1 && keys[0] > "i" && keys[0] < "r", "strictly between: \(keys)")
    }

    @Test func hiddenBlocksStayOut() {
        let editor = DocEditor(docID: "d", baseEpoch: 1, blocks: [
            Self.b("fm", "---\ntags: [a]\n---", key: "i"),
            Self.b("p", "para", key: "r"),
            Self.b("cm", "a comment", parent: "p", key: "i", type: .comment),
            Self.b("q", "qara", key: "v"),
        ])
        let (blocks, baseline) = Surface.load(editor)
        let hidden = Surface.hidden(editor)
        #expect(blocks.map(\.id) == ["p", "q"])
        #expect(hidden.map(\.id) == ["fm", "cm"])
        #expect(hidden.map(\.after) == [nil, "p"])
        let s = Surface.document(blocks)
        let known = Set(baseline.map(\.id))
        #expect(DocDiff.computeOps(baseline: baseline, entries: Surface.entries(s, known: known), hidden: hidden).isEmpty, "untouched: nothing")
        Surface.replace(s, NSRange(location: 4, length: 0), with: "!").apply(to: s)
        #expect(DocDiff.computeOps(baseline: baseline, entries: Surface.entries(s, known: known), hidden: hidden) == [.replace(target: "p", content: "para!")])
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
        h.replace(NSRange(location: from, length: to - from), with: "")
        #expect(h.ids == ["a"])
        #expect(h.markdown == ["almma"])
        #expect(Set(h.ops()) == [.delete(target: "b"), .delete(target: "c"), .replace(target: "a", content: "almma")])
    }

    @Test func deletingAWholeLineDeletesThatBlockOnly() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "bravo", key: "m"), Self.b("c", "charlie", key: "r")])
        h.replace(NSRange(location: h.location(of: "bravo"), length: 6), with: "")
        #expect(h.markdown == ["alpha", "charlie"])
        #expect(h.ids == ["a", "c"])
        #expect(h.ops() == [.delete(target: "b")])
    }

    @Test func deletingAHeadingLineLeavesTheParagraphAParagraph() {
        var h = Harness([Self.b("h", "## Head", key: "i", type: .heading), Self.b("p", "body", parent: "h", key: "i")])
        h.replace(NSRange(location: 0, length: 5), with: "")
        #expect(h.markdown == ["body"])
        #expect(h.ids == ["p"])
    }

    @Test func deletingTheLastListItemLeavesTheNextParagraphAlone() {
        var h = Harness([Self.b("l", "- one\n- two", key: "i"), Self.b("p", "after", key: "r")])
        let two = h.location(of: "two")
        let lineStart = two - 2 // the marker
        h.replace(NSRange(location: lineStart, length: h.location(of: "after") - lineStart), with: "")
        #expect(h.markdown == ["- one", "after"])
        #expect(h.ids == ["l", "p"])
    }

    @Test func aJoinNeverTurnsMarkersIntoText() {
        var h = Harness([Self.b("a", "para", key: "i"), Self.b("l", "- one\n- two", key: "r")])
        let from = h.location(of: "ra")
        let to = h.location(of: "one") - 2 // the start of the item line, before its marker
        h.replace(NSRange(location: from, length: to - from), with: "")
        #expect(h.markdown.first == "paone")
        #expect(!h.text.contains("•\tone"))
        #expect(h.ids == ["a", "l"])
        #expect(h.markdown.last == "- two")
    }

    @Test func forwardDeleteBeforeANonTextBlockSelectsIt() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("c", "```\ncode\n```", key: "m", type: .code), Self.b("z", "zed", key: "r")])
        h.sel = NSRange(location: 5, length: 0)
        h.forwardDelete()
        #expect(h.ids == ["a", "c", "z"])
        #expect(h.sel == NSRange(location: h.location(of: Surface.attachment), length: 1))
        #expect(h.ops().isEmpty)
    }

    @Test func typingOverAJoinAfterANonTextBlockKeepsTheParagraph() {
        var h = Harness([Self.b("c", "```\ncode\n```", key: "i", type: .code), Self.b("n", "November", key: "r")])
        // from just after the U+FFFC (its line break) into the next paragraph
        let from = h.location(of: Surface.attachment) + 1
        h.replace(NSRange(location: from, length: h.location(of: "ember") - from), with: "x")
        #expect(h.markdown == ["```\ncode\n```", "xember"])
        #expect(h.ids == ["c", "n"])
        #expect(h.ops() == [.replace(target: "n", content: "xember")])
    }

    @Test func deletingThroughANonTextBlockFromMidTextKeepsTheNextParagraph() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("c", "```\ncode\n```", key: "m", type: .code), Self.b("z", "zed", key: "r")])
        let from = h.location(of: "pha")
        let r = Surface.expandDeletion(NSRange(location: from, length: h.location(of: Surface.attachment) + 1 - from), in: h.s)
        h.replace(r, with: "")
        #expect(h.markdown == ["al", "zed"])
        #expect(h.ids == ["a", "z"])
        #expect(Set(h.ops()) == [.delete(target: "c"), .replace(target: "a", content: "al")])
    }

    /// A paste or drop at every position of a mixed doc never splits a
    /// block into duplicate ids, never re-mints one, never loses one.
    @Test func pastingAnywhereKeepsIDsWhole() {
        let docs = [
            [Self.b("p", "para", key: "i"), Self.b("l", "- one\n- two\n- three", key: "m"), Self.b("q", "> a quote", key: "q"),
             Self.b("c", "```\ncode\n```", key: "u", type: .code), Self.b("e", "end", key: "x")],
            // a non-text block first and last
            [Self.b("c", "```\ncode\n```", key: "i", type: .code), Self.b("p", "para", key: "m"), Self.b("l", "- one\n- two\n- three", key: "q"),
             Self.b("q", "> a quote", key: "u"), Self.b("e", "```\nlast\n```", key: "x", type: .code)],
        ]
        for doc in docs {
        let length = Harness(doc).s.length
        for viaPaste in [false, true] {
        for text in ["x\ny\nz", "# Head\n\n- a\n- b", "plain", "\n", "**bold** [[Doc]]"] {
            for at in 0..<length {
                var h = Harness(doc)
                // the block the caret is in may change (unless it's a
                // non-text block); every other keeps its content
                let before = Dictionary(uniqueKeysWithValues: h.blocks.map { ($0.id, $0.content.markdown) })
                let here = Surface.block(at: at, in: h.s)
                let mayChange: BlockID? = here.flatMap { $0.content.isRaw ? nil : $0.id }
                defer {
                    let after = Dictionary(uniqueKeysWithValues: h.blocks.map { ($0.id, $0.content.markdown) })
                    for (id, md) in before where id != mayChange {
                        #expect(after[id] == md, "\(text.debugDescription) at \(at): \(id) was \(md.debugDescription), now \(after[id]?.debugDescription ?? "gone")")
                    }
                }
                Surface.$allowsRepair.withValue(true) {
                    if viaPaste {
                        h.apply(Surface.paste(h.s, NSRange(location: at, length: 0), markdown: text, mint: h.mint))
                    } else {
                        h.replace(NSRange(location: at, length: 0), with: text)
                    }
                }
                let stray = Set(h.ids).subtracting(h.minter.allowed)
                #expect(stray.isEmpty, "\(text.debugDescription) at \(at) (\(String(h.s.string.prefix(at)).debugDescription)): \(h.markdown)")
                for id in ["p", "l", "q", "c", "e"] {
                    #expect(h.ids.contains(id), "\(text.debugDescription) at \(at): \(id) kept, ids \(h.ids)")
                }
            }
        }
        }
        }
    }

    @Test func pastingAtTheEndOfANonTextBlocksLineAddsParagraphsAfterIt() {
        for (text, want) in [("x\ny", ["x", "y"]), ("x", ["x"]), ("x\n", ["x"])] {
            var h = Harness([Self.b("c", "```\ncode\n```", key: "i", type: .code), Self.b("n", "november", key: "r")])
            h.replace(NSRange(location: h.location(of: Surface.attachment) + 1, length: 0), with: text)
            #expect(h.markdown == ["```\ncode\n```"] + want + ["november"], "\(text.debugDescription)")
            #expect(h.ids.first == "c" && h.ids.last == "n")
            #expect(!h.ops().contains { if case .replace = $0 { true } else { false } }, "nothing existing changes")
        }
    }

    // MARK: copy and paste as markdown

    @Test func copyIsMarkdownAndPasteBringsTheBlocksBack() {
        let a = Harness([Self.b("h", "## Plan", key: "i", type: .heading),
                         Self.b("l", "- **bold** item\n- see [[Other Doc]]", key: "m"),
                         Self.b("c", "```sh\necho hi\n```", key: "q", type: .code),
                         Self.b("p", "end", key: "u")])
        let all = NSRange(location: 0, length: a.location(of: "end") - 1)
        let md = Surface.markdown(of: a.s, range: all)
        #expect(md == "## Plan\n\n- **bold** item\n- see [[Other Doc]]\n\n```sh\necho hi\n```")
        // within one line: just its inline markdown
        #expect(Surface.markdown(of: a.s, range: NSRange(location: a.location(of: "bold"), length: 8)) == "**bold** ite")
        // into another doc, on an empty line: the same blocks, new ids
        var b = Harness([Self.b("x", "first", key: "i"), Self.b("e", "", key: "r")])
        b.sel = NSRange(location: b.blocks[1].range.location, length: 0)
        b.apply(Surface.paste(b.s, b.sel, markdown: md, mint: b.mint))
        #expect(b.markdown == ["first", "## Plan", "- **bold** item\n- see [[Other Doc]]", "```sh\necho hi\n```"])
        #expect(b.ids[0] == "x" && b.ids[1] == "e", "the empty line's block takes the first part")
    }

    @Test func pastingAtALineStartInsertsBlocksBeforeIt() {
        var h = Harness([Self.b("x", "keep me", key: "i")])
        h.apply(Surface.paste(h.s, NSRange(location: 0, length: 0), markdown: "- one\n- two", mint: h.mint))
        #expect(h.markdown == ["- one\n- two", "keep me"])
        #expect(h.ids.last == "x")
    }

    @Test func pastingInsideALineKeepsInlineFormatting() {
        var h = Harness([Self.b("x", "a  b", key: "i")])
        h.apply(Surface.paste(h.s, NSRange(location: 2, length: 0), markdown: "**bold** and [[Doc]]", mint: h.mint))
        #expect(h.markdown == ["a **bold** and [[Doc]] b"])
        #expect(h.ids == ["x"])
    }

    @Test func deletingAWholeLineAboveANonTextBlockDeletesThatBlock() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("c", "```\ncode\n```", key: "m", type: .code), Self.b("z", "zed", key: "r")])
        h.replace(NSRange(location: 0, length: 6), with: "")
        #expect(h.ids == ["c", "z"])
        #expect(h.ops() == [.delete(target: "a")])
    }

    @Test func typingOverASelectedNonTextBlockReplacesIt() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("c", "```\ncode\n```", key: "m", type: .code), Self.b("z", "zed", key: "r")])
        h.replace(NSRange(location: h.location(of: Surface.attachment), length: 1), with: "x")
        #expect(h.markdown == ["alpha", "x", "zed"])
        #expect(h.ids[0] == "a" && h.ids[2] == "z" && h.ids[1] != "c")
        #expect(h.ops().contains(.delete(target: "c")))
    }

    @Test func aRawHeadingKeepsItsSection() {
        let blocks = [Self.b("h", "## Head <b>x</b>", key: "i", type: .heading), Self.b("p", "body", parent: "h", key: "i")]
        var h = Harness(blocks)
        #expect(h.blocks[0].content.isRaw)
        #expect(h.blocks[0].level == 2)
        h.sel = NSRange(location: h.location(of: "body") + 4, length: 0)
        h.type("!")
        #expect(h.ops() == [.replace(target: "p", content: "body!")])
    }

    @Test func deletingTheLineBreakAtALineEndJoins() {
        var h = Harness([Self.b("a", "alpha", key: "i"), Self.b("b", "beta", key: "m")])
        h.replace(NSRange(location: 5, length: 1), with: "")
        #expect(h.ids == ["a"])
        #expect(h.markdown == ["alphabeta"])
    }

    @Test func selectAllAndDeleteLeavesOneLine() {
        var h = Harness(Self.sample)
        h.replace(NSRange(location: 0, length: h.s.length - 1), with: "")
        #expect(h.blocks.count == 1)
        #expect(h.ids == ["h"])
        #expect(h.text == "\n")
    }

    @Test func pastingLinesMidParagraph() {
        var h = Harness([Self.b("a", "start end", key: "i")])
        h.sel = NSRange(location: 6, length: 0)
        h.replace(h.sel, with: "one\ntwo\nthree ")
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
        h.replace(h.sel, with: "\nx\ny")
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
        h.replace(r, with: "")
        #expect(!h.ids.contains("c"))
        #expect(h.ids.contains("q"))
        #expect(h.ops().contains(.delete(target: "c")))
    }

    @Test func textNeverJoinsANonTextLine() {
        var h = Harness(Self.sample)
        // delete the line break between the code block and the quote
        let c = h.blocks[3]
        h.replace(NSRange(location: c.range.location + 1, length: 1), with: "")
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
        h.replace(NSRange(location: 2, length: 10), with: "Z")
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
        Surface.$allowsRepair.withValue(true) { _ = Surface.normalize(s, selection: &sel) }
        let ids = Surface.blocks(s).map(\.id)
        #expect(ids.count == 3)
        #expect(ids[0] == "x" && ids[1] != "x" && ids[2] == "l")
    }

    @Test func orderedListsRenumberAfterADeletion() {
        var h = Harness([Self.b("l", "1. a\n2. b\n3. c", key: "i")])
        let from = h.location(of: "a") + 1
        let to = h.location(of: "b") + 1
        h.replace(NSRange(location: from, length: to - from), with: "")
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
        var sel = h.sel
        Surface.$allowsRepair.withValue(true) { _ = Surface.normalize(h.s, selection: &sel) }
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
