import Foundation
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// The one-surface editor's UIKit half: the real `SurfaceTextView`, driven
/// the way the keyboard drives it (the delegate asked, then insertText /
/// deleteBackward), with saves going to an in-memory outbox.
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct EditorTextTests {
    @MainActor final class Harness {
        let model: SurfaceModel
        let cache: Cache

        init(_ blocks: [Block], editable: Bool = true) throws {
            cache = try Cache.inMemory()
            model = SurfaceModel(docID: "d", editor: DocEditor(docID: "d", baseEpoch: 1, blocks: blocks), cache: cache, api: nil, app: nil,
                                 editable: editable, saveDelay: .milliseconds(50))
        }

        var tv: SurfaceTextView { model.textView }
        var text: String { tv.text }
        var blocks: [Surface.SurfaceBlock] { Surface.blocks(tv.textStorage) }
        var ids: [BlockID] { blocks.map(\.id) }
        var markdown: [String] { blocks.map(\.content.markdown) }

        func location(of s: String) -> Int { (tv.text as NSString).range(of: s).location }

        /// Each character the way the keyboard sends it.
        func type(_ s: String, at location: Int? = nil) {
            if let location { tv.selectedRange = NSRange(location: location, length: 0) }
            for ch in s {
                if model.textView(tv, shouldChangeTextIn: tv.selectedRange, replacementText: String(ch)) {
                    tv.insertText(String(ch))
                }
            }
        }

        func backspace(at location: Int) {
            tv.selectedRange = NSRange(location: location, length: 0)
            tv.deleteBackward()
        }

        func queued() async throws -> [ProposeRequest] {
            try await cache.pendingOutbox().compactMap { try? JSONDecoder().decode(ProposeRequest.self, from: $0.body ?? Data()) }
        }

        func queuedOps() async throws -> [BlockOp] { try await queued().flatMap { $0.ops.map(\.kind) } }

        /// The text view in a window (first responder, marked text).
        func inWindow() throws -> UIWindow {
            let scene = try #require(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
            let window = UIWindow(windowScene: scene)
            window.addSubview(tv)
            tv.frame = CGRect(x: 0, y: 0, width: 360, height: 400)
            window.makeKeyAndVisible()
            return window
        }
    }

    func blocks(_ contents: String...) -> [Block] {
        var key: String?
        return contents.enumerated().map { i, c in
            key = OrderKey.between(key, nil)
            let type: BlockType = c.hasPrefix("#") ? .heading : c.hasPrefix("```") ? .code : .paragraph
            return Block(id: "b\(i)", docID: "d", parentID: nil, orderKey: key!, blockType: type, content: c)
        }
    }

    // MARK: typing, Return, Backspace

    @Test func typingReturnAndBackspaceKeepIDs() async throws {
        let h = try Harness(blocks("Hello", "World"))
        h.type(" there", at: 5)
        #expect(h.markdown == ["Hello there", "World"])
        // Return at the end: a new line, then typed into: a new block
        h.type("\nMiddle")
        #expect(h.markdown == ["Hello there", "Middle", "World"])
        #expect(h.ids[0] == "b0" && h.ids[2] == "b1")
        let middle = h.ids[1]
        await h.model.flush()
        let ops = try await h.queuedOps()
        #expect(ops.contains(.replace(target: "b0", content: "Hello there")))
        #expect(ops.contains { if case let .insert(id, nil, _, .paragraph, "Middle") = $0 { id == middle } else { false } })
        // Backspace at the start of "World" joins it onto "Middle": Middle keeps its id
        h.backspace(at: h.location(of: "World"))
        #expect(h.markdown == ["Hello there", "MiddleWorld"])
        #expect(h.ids == ["b0", middle])
        // Return mid-text splits it again: the first half keeps the id
        h.type("\n", at: h.location(of: "World"))
        #expect(h.markdown == ["Hello there", "Middle", "World"])
        #expect(h.ids[1] == middle && h.ids[2] != "b1")
        await h.model.flush()
        let all = try await h.queuedOps()
        #expect(all.contains(.delete(target: "b1")), "b1 was merged away")
    }

    @Test func shortcutsTurnLinesIntoListsAndHeadings() async throws {
        let h = try Harness(blocks(""))
        let id = h.ids[0]
        h.type("## Title")
        #expect(h.markdown == ["## Title"])
        #expect(h.text == "Title\n", "the hashes become the heading's look")
        #expect(h.ids == [id])
        let h2 = try Harness(blocks(""))
        h2.type("- milk\neggs")
        #expect(h2.markdown == ["- milk\n- eggs"], "Return in a list makes the next item")
        h2.type("\n")
        h2.type("\n")
        #expect(h2.markdown.first == "- milk\n- eggs", "Return on an empty item leaves the list")
        #expect(h2.blocks.count == 2)
        let h3 = try Harness(blocks(""))
        h3.type("[ ] call mum")
        #expect(h3.markdown == ["- [ ] call mum"])
    }

    @Test func wikilinksAreAtomic() async throws {
        let h = try Harness(blocks("see [[Roadmap]] now"))
        #expect(h.text == "see Roadmap now\n")
        // the caret can't be inside the link: typing there lands at its nearer edge
        h.type("!", at: 6)
        #expect(h.markdown == ["see ![[Roadmap]] now"])
        // Backspace right after it removes all of it
        h.backspace(at: h.location(of: " now"))
        #expect(h.markdown == ["see ! now"])
    }

    @Test func theCaretNeverSitsInsideAWikilink() throws {
        let h = try Harness(blocks("see [[Roadmap]] now"))
        h.tv.selectedRange = NSRange(location: 6, length: 0)
        h.model.textViewDidChangeSelection(h.tv)
        #expect(h.tv.selectedRange.location == 4)
        h.tv.selectedRange = NSRange(location: 9, length: 0)
        h.model.textViewDidChangeSelection(h.tv)
        #expect(h.tv.selectedRange.location == 11)
        h.type("\n", at: 8)
        #expect(h.markdown.contains { $0.contains("[[Roadmap]]") })
    }

    @Test func returnOverASelectionReplacesItThenSplits() throws {
        let h = try Harness(blocks("hello cruel world"))
        h.tv.selectedRange = NSRange(location: 5, length: 6) // " cruel"
        h.type("\n")
        #expect(h.markdown == ["hello", "world"])
        #expect(h.ids[0] == "b0")
    }

    @Test func pastingLinesMakesParagraphs() throws {
        let h = try Harness(blocks("start end"))
        UIPasteboard.general.string = "one\ntwo\nthree "
        h.tv.selectedRange = NSRange(location: 6, length: 0)
        #expect(h.model.handlePaste())
        #expect(h.markdown == ["start one", "two", "three end"])
        #expect(h.ids[0] == "b0")
    }

    @Test func pastingMarkdownIntoAnEmptyLineMakesBlocks() throws {
        let h = try Harness(blocks("a", ""))
        UIPasteboard.general.string = "# Head\n\n- x\n- y\n\n```swift\nlet z = 1\n```"
        h.tv.selectedRange = NSRange(location: h.blocks[1].range.location, length: 0)
        #expect(h.model.handlePaste())
        #expect(h.markdown == ["a", "# Head", "- x\n- y", "```swift\nlet z = 1\n```"])
        #expect(h.ids[1] == "b1", "the empty line's block takes the first")
    }

    // MARK: undo

    @Test func undoAndRedoKeepIDs() throws {
        let h = try Harness(blocks("alpha", "beta"))
        guard let undo = h.tv.undoManager else { Issue.record("no undo manager"); return }
        h.backspace(at: h.location(of: "beta"))
        #expect(h.ids == ["b0"])
        #expect(undo.canUndo)
        undo.undo()
        #expect(h.ids == ["b0", "b1"])
        #expect(h.markdown == ["alpha", "beta"])
        undo.redo()
        #expect(h.ids == ["b0"])
        #expect(h.markdown == ["alphabeta"])
        undo.undo()
        // a split undone
        h.type("\n", at: 2)
        #expect(h.markdown == ["al", "pha", "beta"])
        undo.undo()
        #expect(h.ids == ["b0", "b1"])
        #expect(h.markdown == ["alpha", "beta"])
    }

    @Test func undoAfterTypingThenASplit() throws {
        // in a window, first responder: UIKit's own typing undo is live
        let h = try Harness(blocks("ab"))
        let window = try h.inWindow()
        defer { window.isHidden = true }
        h.tv.becomeFirstResponder()
        guard let undo = h.tv.undoManager else { Issue.record("no undo manager"); return }
        // each keystroke its own event, as on a device
        func tick() { RunLoop.current.run(until: Date().addingTimeInterval(0.05)) }
        h.type("c", at: 2)
        tick()
        let typingUndoable = undo.canUndo
        h.type("\n")
        tick()
        h.type("d")
        tick()
        #expect(h.markdown == ["abc", "d"])
        var steps: [[String]] = []
        while undo.canUndo, steps.count < 10 {
            undo.undo()
            tick()
            steps.append(h.markdown)
        }
        #expect(typingUndoable, "typing is on the surface's undo stack")
        #expect(h.markdown == ["ab"], "undo steps: \(steps)")
        #expect(h.ids == ["b0"])
        h.tv.resignFirstResponder()
    }

    // MARK: fix round 1

    @Test func cutAndPasteOfWholeBlocksIsAMove() async throws {
        let h = try Harness(blocks("one", "two", "three", ""))
        let two = h.blocks[1].range
        h.tv.selectedRange = two
        #expect(h.model.handleCut())
        #expect(h.markdown == ["one", "three", ""])
        h.tv.selectedRange = NSRange(location: h.blocks[2].range.location, length: 0)
        #expect(h.model.handlePaste())
        #expect(h.markdown == ["one", "three", "two", ""])
        #expect(h.ids.prefix(3) == ["b0", "b2", "b1"])
        await h.model.flush()
        let ops = try await h.queuedOps()
        #expect(!ops.contains { if case .insert = $0 { true } else { false } }, "no insert: \(ops)")
        #expect(!ops.contains(.delete(target: "b1")))
        #expect(ops.contains { if case .move = $0 { true } else { false } })
    }

    @Test func aSecondPasteIsNewText() throws {
        let h = try Harness(blocks("one", "two", ""))
        h.tv.selectedRange = h.blocks[1].range
        _ = h.model.handleCut()
        h.tv.selectedRange = NSRange(location: h.blocks[1].range.location, length: 0)
        _ = h.model.handlePaste()
        h.tv.selectedRange = NSRange(location: h.blocks[2].range.location, length: 0)
        _ = h.model.handlePaste()
        #expect(Set(h.ids).count == h.ids.count, "no duplicate ids")
    }

    @Test func undoSurvivesARefreshOfAnotherBlock() throws {
        let h = try Harness(blocks("first", "second", "third"))
        guard let undo = h.tv.undoManager else { Issue.record("no undo manager"); return }
        h.type("\n", at: h.location(of: "third") + 2) // a split, undoable
        #expect(h.markdown == ["first", "second", "th", "ird"])
        let remote = [
            Block(id: "b0", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "first, longer now"),
            Block(id: "b1", docID: "d", parentID: nil, orderKey: "r", blockType: .paragraph, content: "second"),
            Block(id: "b2", docID: "d", parentID: nil, orderKey: "v", blockType: .paragraph, content: "third"),
        ]
        h.model.applyRemote(DocEditor(docID: "d", baseEpoch: 2, blocks: remote + [Block(id: h.ids[3], docID: "d", parentID: nil, orderKey: "x", blockType: .paragraph, content: "ird")]))
        #expect(h.markdown.first == "first, longer now")
        #expect(undo.canUndo, "the split is still undoable")
        undo.undo()
        #expect(h.markdown == ["first, longer now", "second", "third"])
    }

    @Test func undoOfABlockARefreshChangedIsDropped() async throws {
        let h = try Harness(blocks("first", "second"))
        guard let undo = h.tv.undoManager else { Issue.record("no undo manager"); return }
        h.type("\n", at: 2)
        await h.model.flush() // saved: the refresh may take theirs
        let remote = [
            Block(id: "b0", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "FI"),
            Block(id: h.ids[1], docID: "d", parentID: nil, orderKey: "m", blockType: .paragraph, content: "rst"),
            Block(id: "b1", docID: "d", parentID: nil, orderKey: "r", blockType: .paragraph, content: "second"),
        ]
        h.model.applyRemote(DocEditor(docID: "d", baseEpoch: 2, blocks: remote))
        #expect(h.markdown.first == "FI")
        #expect(!undo.canUndo, "that step would undo text that isn't there")
    }

    @Test func aViewerCantUndo() async throws {
        let h = try Harness(blocks("a"))
        guard let undo = h.tv.undoManager else { Issue.record("no undo manager"); return }
        h.type("\n", at: 1)
        await h.model.flush()
        h.model.setEditable(false)
        #expect(!undo.canUndo)
        h.model.deleteBlock("b0")
        h.model.saveSource("b0", "changed")
        #expect(h.markdown == ["a", ""])
    }

    @Test func aToDoTickKeepsOtherTypingUnsaved() async throws {
        let h = try Harness(blocks("- [ ] one\n- [ ] two"))
        var ticked: [(BlockID, Int, Bool)] = []
        h.model.onTodoCheckbox = { ticked.append(($0, $1, $2)) }
        h.type("!", at: h.location(of: "one") + 3)
        h.model.setChecked(line: 1, true)
        #expect(ticked.count == 1 && ticked[0].1 == 1)
        await h.model.flush()
        #expect(try await h.queuedOps() == [.replace(target: "b0", content: "- [ ] one!\n- [x] two")], "the typing still saves")
    }

    @Test func nothingSavesMidComposition() async throws {
        let h = try Harness(blocks("ab"))
        let window = try h.inWindow()
        defer { window.isHidden = true }
        h.tv.becomeFirstResponder()
        h.tv.selectedRange = NSRange(location: 2, length: 0)
        h.tv.setMarkedText("か", selectedRange: NSRange(location: 1, length: 0))
        h.model.save()
        try await Task.sleep(for: .milliseconds(30))
        #expect(try await h.queued().isEmpty)
        h.tv.unmarkText()
        h.tv.resignFirstResponder()
    }

    @Test func aReadOnlyCanvasRefreshes() throws {
        let canvas = Block(id: "cv", docID: "d", parentID: nil, orderKey: "i", blockType: .canvasScene, content: "{\"v\":1}")
        let h = try Harness([canvas], editable: false)
        #expect(h.text.contains(Surface.attachment))
        var changed = canvas
        changed.content = "{\"v\":2}"
        h.model.applyRemote(DocEditor(docID: "d", baseEpoch: 2, blocks: [changed]))
        #expect(h.blocks.first?.content.markdown == "{\"v\":2}")
    }

    @Test func typingOverASelectedBlockReplacesIt() throws {
        let h = try Harness(blocks("a", "```\ncode\n```", "z"))
        h.tv.selectedRange = NSRange(location: h.location(of: Surface.attachment), length: 1)
        #expect(!h.model.textView(h.tv, shouldChangeTextIn: h.tv.selectedRange, replacementText: "x"))
        #expect(h.markdown == ["a", "x", "z"])
    }

    @Test func aRangedSelectionNeverEndsInsideAMarker() throws {
        let h = try Harness(blocks("para", "- one"))
        let one = h.location(of: "one")
        h.tv.selectedRange = NSRange(location: 2, length: one - 1 - 2) // ends between • and the tab
        h.model.textViewDidChangeSelection(h.tv)
        #expect(h.tv.selectedRange.location + h.tv.selectedRange.length == one)
    }

    // MARK: fix round 2

    @Test func pastingCutBlocksInsideAListIsPlainText() async throws {
        let h = try Harness(blocks("para", "- one\n- two\n- three"))
        h.tv.selectedRange = h.blocks[0].range
        #expect(h.model.handleCut())
        h.tv.selectedRange = NSRange(location: h.location(of: "two"), length: 0)
        #expect(h.model.handlePaste())
        #expect(Set(h.ids).count == h.ids.count, "no list split into two runs with one id: \(h.ids)")
        #expect(h.markdown.contains { $0.contains("para") })
    }

    /// Every place a paste or a drop can land that could split a block.
    @Test func pastesAndDropsNeverSplitABlockIntoDuplicateIDs() throws {
        let doc = ["para", "- one\n- two\n- three", "> a quote", "```\ncode\n```", "end"]
        let texts = ["x\ny\nz", "# Head\n\n- a\n- b", "plain"]
        func spots(_ h: Harness) -> [Int] {
            [h.location(of: "two"), h.location(of: "two") + 1, h.location(of: "three") + 5,
             h.location(of: "a quote"), h.location(of: "quote"),
             h.location(of: Surface.attachment), h.location(of: Surface.attachment) + 1, h.location(of: "end")]
        }
        for text in texts {
            for i in spots(try Harness(blocks(doc[0], doc[1], doc[2], doc[3], doc[4]))).indices {
                for via in ["paste", "drop"] {
                    let h = try Harness(blocks(doc[0], doc[1], doc[2], doc[3], doc[4]))
                    let at = spots(h)[i]
                    if via == "paste" {
                        UIPasteboard.general.string = text
                        h.tv.selectedRange = NSRange(location: at, length: 0)
                        _ = h.model.handlePaste()
                    } else {
                        h.model.dropText(text, at: at)
                    }
                    #expect(Set(h.ids).count == h.ids.count, "\(via) \(text.debugDescription) at \(i): \(h.ids)")
                    #expect(h.ids.contains("b1") && h.ids.contains("b3"), "\(via) at \(i): the list and the code block keep their ids")
                }
            }
        }
    }

    @Test func undoNeverPutsBackABlockARefreshResurrected() async throws {
        let h = try Harness(blocks("alpha", "x-ray", "zulu"))
        guard let undo = h.tv.undoManager else { Issue.record("no undo manager"); return }
        // delete the whole x-ray line, save it
        let x = h.blocks[1].range
        #expect(!h.model.textView(h.tv, shouldChangeTextIn: x, replacementText: ""))
        #expect(h.ids == ["b0", "b2"])
        await h.model.flush()
        // the server brings it back (the delete was refused, or re-added)
        let remote = [
            Block(id: "b0", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "alpha"),
            Block(id: "b1", docID: "d", parentID: nil, orderKey: "r", blockType: .paragraph, content: "x-ray"),
            Block(id: "b2", docID: "d", parentID: nil, orderKey: "v", blockType: .paragraph, content: "zulu"),
        ]
        h.model.applyRemote(DocEditor(docID: "d", baseEpoch: 2, blocks: remote))
        #expect(h.ids == ["b0", "b1", "b2"])
        var steps = 0
        while undo.canUndo, steps < 5 { undo.undo(); steps += 1 }
        #expect(Set(h.ids).count == h.ids.count, "no duplicate: \(h.ids)")
    }

    @Test func aDropFindsItsPlaceAfterTheTextChanged() throws {
        let h = try Harness(blocks("alpha", "bravo"))
        let s = h.tv.textStorage
        let b = try #require(Surface.block(at: h.location(of: "bravo"), in: s))
        let anchor = (b.id, 2)
        h.type("XXXX", at: 0) // the text moves while the drop loads
        let at = h.model.resolve(anchor, fallback: 0)
        #expect(at == h.location(of: "bravo") + 2)
    }

    @Test func writingToolsAreOff() throws {
        let h = try Harness(blocks("a"))
        #expect(h.tv.writingToolsBehavior == .none)
    }

    // MARK: non-text blocks

    @Test func codeBlocksAreAttachments() async throws {
        let h = try Harness(blocks("before", "```swift\nlet x = 1\n```", "after"))
        #expect(h.text == "before\n\u{FFFC}\nafter\n")
        #expect(h.tv.textStorage.attribute(.attachment, at: h.location(of: "\u{FFFC}"), effectiveRange: nil) is BlockAttachment)
        // its source, edited in the sheet
        h.model.editSource("b1")
        #expect(h.model.sourceEditing == "b1")
        h.model.saveSource("b1", "```swift\nlet x = 2\n```")
        await h.model.flush()
        #expect(try await h.queuedOps() == [.replace(target: "b1", content: "```swift\nlet x = 2\n```")])
        // Backspace at the start of the line after it selects it; again deletes it
        h.backspace(at: h.location(of: "after"))
        #expect(h.tv.selectedRange == NSRange(location: h.location(of: "\u{FFFC}"), length: 1))
        if h.model.textView(h.tv, shouldChangeTextIn: h.tv.selectedRange, replacementText: "") { h.tv.deleteBackward() }
        #expect(h.markdown == ["before", "after"])
        await h.model.flush()
        #expect(try await h.queuedOps().contains(.delete(target: "b1")))
    }

    @Test func typingBesideANonTextBlockMakesAParagraph() throws {
        let h = try Harness(blocks("```\ncode\n```"))
        h.type("x", at: 1)
        #expect(h.markdown == ["```\ncode\n```", "x"])
        #expect(h.ids[0] == "b0")
    }

    // MARK: checkboxes, kinds

    @Test func tickingABox() async throws {
        let h = try Harness(blocks("- [ ] one\n- [ ] two"))
        let ls = Surface.lines(h.tv.textStorage)
        h.model.setChecked(line: 1, true)
        #expect(h.markdown == ["- [ ] one\n- [x] two"])
        #expect(ls.count == 2)
        await h.model.flush()
        #expect(try await h.queuedOps() == [.replace(target: "b0", content: "- [ ] one\n- [x] two")])
    }

    @Test func kindChangesFromTheBar() throws {
        let h = try Harness(blocks("alpha"))
        h.tv.selectedRange = NSRange(location: 2, length: 0)
        h.model.setKind(.heading2)
        #expect(h.markdown == ["## alpha"])
        h.model.toggleTodo()
        #expect(h.markdown == ["- [ ] alpha"])
        #expect(h.ids == ["b0"])
    }

    @Test func boldOverASelection() throws {
        let h = try Harness(blocks("make this bold"))
        h.tv.selectedRange = NSRange(location: 10, length: 4)
        h.model.toggle(.bold)
        #expect(h.markdown == ["make this **bold**"])
    }

    // MARK: saving

    @Test func savesCoalesceIntoOneWrite() async throws {
        let h = try Harness(blocks("a"))
        for ch in "bcdef" {
            h.type(String(ch), at: h.blocks[0].range.length - 1)
            try await Task.sleep(for: .milliseconds(80))
        }
        await h.model.flush()
        let rows = try await h.queued()
        #expect(rows.count == 1, "one block typed in: one queued write")
        #expect(rows.first?.ops.map(\.kind) == [.replace(target: "b0", content: "abcdef")])
    }

    /// A save is in the outbox before the network answers.
    @Test func savesPersistWhileTheNetworkHangs() async throws {
        let h = try Harness(blocks("a"))
        let gate = Gate()
        h.model.replay = { await gate.wait() }
        h.type("b", at: 1)
        await h.model.flush()
        #expect(try await h.cache.pendingOutbox().count == 1, "written while the first send still hangs")
        h.type("c", at: 2)
        await h.model.flush()
        let last = try await h.queued().last
        #expect(last?.ops.first?.kind == .replace(target: "b0", content: "abc"))
        #expect(gate.waiting > 0, "the send really is still hanging")
        gate.open()
    }

    /// A failing outbox write is retried; the text is never dropped.
    @Test func aFailingOutboxWriteIsRetriedNotDropped() async throws {
        let h = try Harness(blocks("a"))
        h.model.persistRetryDelays = [.milliseconds(10), .milliseconds(10)]
        let failures = Locked(0)
        let real = h.model.persist
        h.model.persist = { req, c, k in
            if failures.value < 2 { failures.mutate { $0 += 1 }; throw CocoaError(.fileWriteUnknown) }
            try await real(req, c, k)
        }
        h.type("b", at: 1)
        await h.model.flush()
        #expect(try await h.cache.pendingOutbox().count == 1, "written on the third try")
        h.model.persist = { _, _, _ in throw CocoaError(.fileWriteOutOfSpace) }
        h.type("c", at: 2)
        await h.model.flush()
        #expect(h.model.persistFailures == 1)
        #expect(h.model.chip.text == "Couldn't save · Retry")
        #expect(h.markdown == ["abc"], "still there")
        h.model.persist = real
        await h.model.flush()
        #expect(h.model.persistFailures == 0)
        #expect(try await h.queued().last?.ops.first?.kind == .replace(target: "b0", content: "abc"))
    }

    // MARK: remote changes

    /// A sync refresh merges in: an untouched block takes theirs, the caret
    /// stays on its text, unsaved typing stays.
    @Test func aRemoteChangeKeepsTheCaretAndTypedText() async throws {
        let h = try Harness(blocks("first", "second", "third"))
        h.type("!", at: h.location(of: "third") + 5)
        let caretBefore = h.tv.selectedRange.location - h.blocks[2].range.location
        let remote = [
            Block(id: "b0", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "first, much longer now"),
            Block(id: "b1", docID: "d", parentID: nil, orderKey: "r", blockType: .paragraph, content: "second"),
            Block(id: "b2", docID: "d", parentID: nil, orderKey: "v", blockType: .paragraph, content: "third"),
        ]
        h.model.applyRemote(DocEditor(docID: "d", baseEpoch: 2, blocks: remote))
        #expect(h.markdown == ["first, much longer now", "second", "third!"])
        #expect(h.tv.selectedRange.location - h.blocks[2].range.location == caretBefore, "the caret stays on its text")
        await h.model.flush()
        #expect(try await h.queuedOps() == [.replace(target: "b2", content: "third!")])
    }

    /// Changed on another device while typed in: saved flagged on the old
    /// epoch; Take theirs drops our queued save and shows their text.
    @Test func takeTheirsDropsOurQueuedSave() async throws {
        let h = try Harness(blocks("mine"))
        h.type("!", at: 4)
        let remote = [Block(id: "b0", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "theirs")]
        h.model.applyRemote(DocEditor(docID: "d", baseEpoch: 2, blocks: remote))
        #expect(h.model.conflicts["b0"] == SurfaceConflict(base: 1, theirs: "theirs"))
        #expect(h.markdown == ["mine!"])
        await h.model.flush()
        #expect(try await h.cache.conflictedBlocks("d") == ["b0": 1], "saved on its old epoch, flagged")
        await h.model.takeTheirs("b0")
        #expect(h.markdown == ["theirs"])
        #expect(try await h.cache.pendingOutbox().isEmpty)
        #expect(try await h.cache.conflictedBlocks("d").isEmpty)
    }

    // MARK: composition

    @Test func markedTextIsLeftAlone() async throws {
        let h = try Harness(blocks(""))
        let window = try h.inWindow()
        defer { window.isHidden = true }
        h.tv.becomeFirstResponder()
        h.tv.setMarkedText("## ", selectedRange: NSRange(location: 3, length: 0))
        #expect(h.tv.markedTextRange != nil, "the composition really opened")
        #expect(h.text.hasPrefix("## "), "not turned into a heading mid-composition")
        h.tv.unmarkText()
        h.tv.resignFirstResponder()
    }

    @Test func flushCommitsAnOpenComposition() async throws {
        let h = try Harness(blocks("ab"))
        let window = try h.inWindow()
        defer { window.isHidden = true }
        h.tv.becomeFirstResponder()
        h.tv.selectedRange = NSRange(location: 2, length: 0)
        h.tv.setMarkedText("か", selectedRange: NSRange(location: 1, length: 0))
        #expect(h.tv.markedTextRange != nil)
        await h.model.flush()
        #expect(try await h.queued().last?.ops.first?.kind == .replace(target: "b0", content: "abか"))
        h.tv.resignFirstResponder()
    }

    // MARK: read-only

    @Test func readOnlyFollowsLinksAndRefusesEdits() throws {
        let h = try Harness(blocks("see [[Roadmap]] and [x](https://x.org)"), editable: false)
        #expect(!h.tv.isEditable)
        #expect(!h.model.textView(h.tv, shouldChangeTextIn: NSRange(location: 0, length: 0), replacementText: "a"))
        let link = h.tv.textStorage.attribute(.link, at: h.location(of: "Roadmap"), effectiveRange: nil) as? URL
        #expect(link.flatMap(InlineMarkdown.wikiTarget) == "Roadmap")
        var opened: String?
        h.model.onOpenWiki = { opened = $0 }
        h.model.follow(try #require(link))
        #expect(opened == "Roadmap")
    }

    @Test func demotedToViewerFlushesThenReadsOnly() async throws {
        let h = try Harness(blocks("a"))
        h.type("b", at: 1)
        await h.model.flush()
        h.model.setEditable(false)
        #expect(!h.tv.isEditable)
        #expect(try await h.queuedOps() == [.replace(target: "b0", content: "ab")])
        #expect(h.tv.textStorage.attribute(.link, at: 0, effectiveRange: nil) == nil)
    }

    /// The text view outlives the model in UIKit's hands: callbacks after
    /// that do nothing.
    @Test func callbacksAfterTheModelIsGoneAreHarmless() throws {
        var harness: Harness? = try Harness(blocks("a"))
        let tv = harness!.tv
        let bar = harness!.model.formattingBar
        weak var gone = harness!.model
        harness = nil
        #expect(gone == nil, "nothing keeps the model alive")
        tv.insertText("b")
        tv.deleteBackward()
        tv.cmdBold()
        tv.cmdIndent()
        bar.update(for: .paragraph(AttributedString("x")), marks: .bold)
        #expect(bar.model == nil)
    }

    @Test func chipSaysWhatHappened() {
        let none = DocOutboxState()
        #expect(EditorChip.make(online: true, unsaved: 0, outbox: none, reviews: []).text == "Saved")
        #expect(EditorChip.make(online: true, unsaved: 1, outbox: none, reviews: []).text == "Saving…")
        #expect(EditorChip.make(online: false, unsaved: 0, outbox: DocOutboxState(pending: 3), reviews: []).text == "Offline · 3 pending")
        #expect(EditorChip.make(online: true, unsaved: 0, outbox: DocOutboxState(pending: 1, liveSession: true), reviews: []).text == "Doc is open in a live session · will retry")
        #expect(EditorChip.make(online: true, unsaved: 0, outbox: none, reviews: [.red]).tone == .conflict)
        #expect(EditorChip.make(online: true, unsaved: 0, outbox: none, reviews: [.yellow]).tone == .review)
        #expect(EditorChip.make(online: true, unsaved: 0, outbox: DocOutboxState(failed: 2), reviews: [.red]).text == "2 edits not saved")
    }
}

/// A send that hangs until the test opens it.
@MainActor final class Gate {
    private var continuations: [CheckedContinuation<Void, Never>] = []
    private var isOpen = false
    var waiting: Int { continuations.count }

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { continuations.append($0) }
    }

    func open() {
        isOpen = true
        continuations.forEach { $0.resume() }
        continuations = []
    }
}

/// A tiny lock for test closures.
final class Locked<T: Sendable>: @unchecked Sendable {
    private var v: T
    private let lock = NSLock()
    init(_ v: T) { self.v = v }
    var value: T { lock.withLock { v } }
    func mutate(_ f: (inout T) -> Void) { lock.withLock { f(&v) } }
}
