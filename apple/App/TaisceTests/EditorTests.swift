import Foundation
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// The editor's UIKit half: the text bridge, and a real block text view
/// driven the way the keyboard drives it (insertText / deleteBackward).
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct EditorTextTests {
    func p(_ s: String) -> EditorBlockContent { .paragraph(AttributedString(s)) }

    @Test func bridgeRoundTripsEveryKind() throws {
        let samples = [
            "**bold** *it* `code` ~~gone~~ [link](https://x.org) [[Doc|alias]] 🎉 中文 مرحبا",
            "## Heading with *style*",
            "> a quote\n> two lines",
            "- [ ] task\n  - nested **bold**\n1. numbered",
            "```swift\nlet x = 1\n```",
        ]
        for md in samples {
            let content = EditorBlockContent.parse(markdown: md)
            let text = EditorText.attributed(content, traits: nil)
            let back = EditorText.content(from: text, like: content)
            #expect(back == content, "\(md)")
            #expect(back.markdown == content.markdown)
        }
    }

    @Test func listMarkersAndCarets() throws {
        let content = EditorBlockContent.parse(markdown: "- [ ] one\n- two")
        let text = EditorText.attributed(content, traits: nil)
        #expect(text.string == "☐\tone\n•\ttwo")
        let lines = EditorText.lines(text)
        #expect(lines.map(\.markerLength) == [2, 2])
        #expect(EditorText.caret(at: 7, in: text, kind: .list) == EditorCaret(line: 1, offset: 0))
        #expect(EditorText.caret(at: 1, in: text, kind: .list) == EditorCaret(line: 0, offset: 0), "inside a marker counts as its start")
        #expect(EditorText.location(of: EditorCaret(line: 1, offset: 3), in: text, kind: .list) == 11)
        #expect(EditorText.textBeforeCaret(10, in: text, kind: .list) == "tw")
        // a line that lost its marker continues the list
        let m = NSMutableAttributedString(attributedString: text)
        m.append(NSAttributedString(string: "\nthree"))
        guard case let .list(items) = EditorText.content(from: m, like: content) else { Issue.record("a list"); return }
        #expect(items.count == 3 && items[2].prefix.checkbox == nil && String(items[2].text.characters) == "three")
    }

    // MARK: a real text view

    @MainActor final class Harness {
        let model: EditorModel
        let cache: Cache
        var views: [BlockID: (BlockTextView, BlockTextCoordinator)] = [:]

        init(_ blocks: [Block]) throws {
            cache = try Cache.inMemory()
            model = EditorModel(docID: "d", editor: DocEditor(docID: "d", baseEpoch: 1, blocks: blocks), cache: cache, api: nil, app: nil, saveDelay: .milliseconds(50))
            sync()
        }

        /// Make (or reload) a view per block, as the SwiftUI list would.
        func sync() {
            for item in model.items {
                if let (_, c) = views[item.id] {
                    if c.revision != item.revision { c.load(item, dynamicType: .large) }
                    c.applyPendingFocus()
                    continue
                }
                let tv = BlockTextView(usingTextLayoutManager: true)
                let c = BlockTextCoordinator(id: item.id, model: model)
                tv.delegate = c
                tv.handler = c
                c.textView = tv
                c.load(item, dynamicType: .large)
                model.register(c)
                views[item.id] = (tv, c)
                c.applyPendingFocus()
            }
        }

        func view(_ id: BlockID) -> BlockTextView { views[id]!.0 }

        /// Each character the way the keyboard sends it: asked of the
        /// delegate first, inserted only if it agrees.
        func type(_ id: BlockID, _ s: String, at location: Int? = nil) {
            if let location { view(id).selectedRange = NSRange(location: location, length: 0) }
            var target = id
            for ch in s {
                let tv = view(target)
                let c = views[target]!.1
                if c.textView(tv, shouldChangeTextIn: tv.selectedRange, replacementText: String(ch)) {
                    tv.insertText(String(ch))
                }
                sync()
                // the caret may have moved to another block (Return)
                if let f = model.focusTarget { target = f }
            }
        }

        func backspace(_ id: BlockID, at location: Int) {
            let tv = view(id)
            tv.selectedRange = NSRange(location: location, length: 0)
            tv.deleteBackward()
            sync()
        }

        var markdown: [String] { model.snapshot.map(\.markdown) }
    }

    func blocks(_ contents: String...) -> [Block] {
        var key: String?
        return contents.enumerated().map { i, c in
            key = OrderKey.between(key, nil)
            return Block(id: "b\(i)", docID: "d", parentID: nil, orderKey: key!, blockType: c.hasPrefix("#") ? .heading : .paragraph, content: c)
        }
    }

    @Test func typingReturnAndBackspace() async throws {
        let h = try Harness(blocks("Hello", "World"))
        h.type("b0", " there", at: 5)
        #expect(h.view("b0").text == "Hello there")
        // Return at the end: a new block after it
        h.type("b0", "\nMiddle")
        #expect(h.model.items.count == 3)
        let new = h.model.items[1].id
        await h.model.flush()
        #expect(h.markdown == ["Hello there", "Middle", "World"])
        // Backspace at the start of "World" joins it onto "Middle"
        h.backspace("b1", at: 0)
        #expect(h.markdown == ["Hello there", "MiddleWorld"])
        // Return mid-text splits it again
        h.type(new, "\n", at: 6)
        #expect(h.markdown == ["Hello there", "Middle", "World"])
        await h.model.flush()
        // the outbox holds the whole story, in order, and replays to the same doc
        let queued = try await h.cache.pendingOutbox().count
        #expect(queued >= 4)
    }

    @Test func shortcutsTurnBlocksIntoListsAndHeadings() async throws {
        let h = try Harness(blocks(""))
        h.type("b0", "## Title")
        #expect(h.markdown == ["## Title"])
        #expect(h.view("b0").text == "Title", "the hashes become the heading's look")
        let h2 = try Harness(blocks(""))
        h2.type("b0", "- milk\neggs")
        #expect(h2.markdown == ["- milk\n- eggs"], "Return in a list makes the next item")
        h2.type("b0", "\n")
        h2.type("b0", "\n")
        #expect(h2.markdown.first == "- milk\n- eggs", "Return on an empty item leaves the list")
        #expect(h2.model.items.count == 2)
        let h3 = try Harness(blocks(""))
        h3.type("b0", "[ ] call mum")
        #expect(h3.markdown == ["- [ ] call mum"])
    }

    @Test func wikilinksAreAtomic() async throws {
        let h = try Harness(blocks("see [[Roadmap]] now"))
        let tv = h.view("b0")
        #expect(tv.text == "see Roadmap now")
        // the caret can't be inside the link: typing there lands at its nearer edge
        h.type("b0", "!", at: 6)
        #expect(h.markdown == ["see ![[Roadmap]] now"])
        // Backspace right after it removes all of it
        h.backspace("b0", at: 12)
        #expect(h.markdown == ["see ! now"])
    }

    @Test func theCaretNeverSitsInsideAWikilink() throws {
        let h = try Harness(blocks("see [[Roadmap]] now"))
        let tv = h.view("b0")
        tv.selectedRange = NSRange(location: 6, length: 0)   // "Ro|admap": nearer the start
        #expect(tv.selectedRange.location == 4)
        tv.selectedRange = NSRange(location: 9, length: 0)   // "Road|map"... nearer the end
        #expect(tv.selectedRange.location == 11)
        // so Return never cuts a link in two
        h.type("b0", "\n", at: 8)
        #expect(h.markdown.first?.contains("[[Roadmap]]") == true)
    }

    @Test func returnOverASelectionReplacesItThenSplits() throws {
        let h = try Harness(blocks("hello cruel world"))
        let tv = h.view("b0")
        tv.selectedRange = NSRange(location: 5, length: 6)    // " cruel"
        h.type("b0", "\n")
        #expect(h.markdown == ["hello", "world"], "the space at the cut goes, as markdown drops it")
    }

    @Test func savesCoalesceIntoOneWrite() async throws {
        let h = try Harness(blocks("a"))
        for ch in "bcdef" {
            h.type("b0", String(ch), at: h.view("b0").text.utf16.count)
            try await Task.sleep(for: .milliseconds(80))
        }
        await h.model.flush()
        let rows = try await h.cache.pendingOutbox()
        #expect(rows.count == 1, "one block typed in: one queued write")
        let req = try JSONDecoder().decode(ProposeRequest.self, from: rows[0].body ?? Data())
        #expect(req.ops.map(\.kind) == [.replace(target: "b0", content: "abcdef")])
    }

    /// A save is in the outbox before the network answers: a hanging
    /// replay never holds typed text in memory, and flush (Done) returns.
    @Test func savesPersistWhileTheNetworkHangs() async throws {
        let h = try Harness(blocks("a"))
        let gate = Gate()
        h.model.replay = { await gate.wait() }
        h.type("b0", "b", at: 1)
        await h.model.flush()
        #expect(try await h.cache.pendingOutbox().count == 1, "written while the first send still hangs")
        h.type("b0", "c", at: 2)
        await h.model.flush()
        let rows = try await h.cache.pendingOutbox()
        let req = try JSONDecoder().decode(ProposeRequest.self, from: rows.last?.body ?? Data())
        #expect(req.ops.first?.kind == .replace(target: "b0", content: "abc"))
        #expect(gate.waiting > 0, "the send really is still hanging")
        gate.open()
    }

    /// A sync refresh between a keystroke's commit and its outbox write
    /// keeps the typed text and the new block.
    @Test func aRefreshBeforeTheWriteLandsKeepsTypedText() async throws {
        let h = try Harness(blocks("hello", "other"))
        try await h.cache.storeDoc(DocTree(doc: DocSummary(id: "d", parentID: nil, title: "D", currentEpoch: 1),
                                           roots: h.model.session.editor.children(of: nil).map { BlockNode(block: $0) }))
        // the editor sync would rebuild from: the cache, before our write, with someone else's edit
        var stale = try #require(try await h.cache.editor(for: "d"))
        _ = try stale.apply(.replaceText("b1", "other, edited remotely"))
        h.type("b0", " world", at: 5)
        h.model.save(["b0"])           // committed, not yet in the outbox
        h.type("b0", "\n")             // a split: its insert isn't written yet either
        let newID = try #require(h.model.focusTarget)
        h.model.applyRemote(stale)
        #expect(h.markdown == ["hello world", "", "other, edited remotely"])
        #expect(h.model.session.item(newID)?.isDraft == false, "the new block is not re-inserted")
        await h.model.flush()
    }

    /// Mid-composition (an input method's marked text) nothing reshapes
    /// the block: no shortcut, no reload.
    @Test func markedTextIsLeftAlone() async throws {
        let h = try Harness(blocks(""))
        let tv = h.view("b0")
        let scene = try #require(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.addSubview(tv)
        tv.frame = CGRect(x: 0, y: 0, width: 300, height: 60)
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        tv.becomeFirstResponder()
        tv.setMarkedText("## ", selectedRange: NSRange(location: 3, length: 0))
        let composing = tv.markedTextRange != nil
        #expect(tv.text == "## ", "the composition is untouched")
        #expect(composing, "the composition really opened")
        #expect(h.model.snapshot == [.paragraph(AttributedString("## "))], "tracked, but not turned into a heading mid-composition")
        tv.unmarkText()
        tv.resignFirstResponder()
    }

    /// Done or a back-swipe frees the model while UIKit still holds the
    /// text view and the bar: callbacks after that do nothing, no crash.
    @Test func callbacksAfterTheEditorIsGoneAreHarmless() throws {
        var harness: Harness? = try Harness(blocks("a"))
        let (tv, c) = harness!.views["b0"]!
        let bar = harness!.model.formattingBar
        weak var gone = harness!.model
        harness = nil
        #expect(gone == nil, "nothing keeps the model alive")
        tv.insertText("b")
        c.textViewDidChange(tv)
        c.textViewDidChangeSelection(tv)
        _ = c.textView(tv, shouldChangeTextIn: NSRange(location: 0, length: 0), replacementText: "\n")
        tv.deleteBackward()
        c.toggle(.bold)
        c.indent(1)
        c.textViewDidEndEditing(tv)
        bar.update(for: .paragraph(AttributedString("x")), marks: .bold)
        #expect(bar.model == nil)
    }

    /// An outbox write that throws is retried; the text is never dropped,
    /// and after the retries it stays (on screen) for the next flush.
    @Test func aFailingOutboxWriteIsRetriedNotDropped() async throws {
        let h = try Harness(blocks("a"))
        h.model.persistRetryDelays = [.milliseconds(10), .milliseconds(10)]
        let failures = Locked(0)
        let real = h.model.persist
        h.model.persist = { req, c, k in
            if failures.value < 2 { failures.mutate { $0 += 1 }; throw CocoaError(.fileWriteUnknown) }
            try await real(req, c, k)
        }
        h.type("b0", "b", at: 1)
        await h.model.flush()
        #expect(try await h.cache.pendingOutbox().count == 1, "written on the third try")
        // failing for good: kept, said, and written by the next flush
        h.model.persist = { _, _, _ in throw CocoaError(.fileWriteOutOfSpace) }
        h.type("b0", "c", at: 2)
        await h.model.flush()
        #expect(h.model.persistFailures == 1)
        #expect(h.model.chip.text == "Couldn't save · Retry")
        #expect(h.markdown == ["abc"], "still there")
        h.model.persist = real
        await h.model.flush()
        #expect(h.model.persistFailures == 0)
        let last = try JSONDecoder().decode(ProposeRequest.self, from: try await h.cache.pendingOutbox().last?.body ?? Data())
        #expect(last.ops.first?.kind == .replace(target: "b0", content: "abc"))
    }

    /// Done (or the app going away) mid-composition saves what's composed.
    @Test func flushCommitsAnOpenComposition() async throws {
        let h = try Harness(blocks("ab"))
        let tv = h.view("b0")
        let scene = try #require(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        window.addSubview(tv)
        tv.frame = CGRect(x: 0, y: 0, width: 300, height: 60)
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        tv.becomeFirstResponder()
        h.model.didFocus(h.views["b0"]!.1)
        tv.selectedRange = NSRange(location: 2, length: 0)
        tv.setMarkedText("か", selectedRange: NSRange(location: 1, length: 0))
        #expect(tv.markedTextRange != nil)
        await h.model.flush()
        let row = try #require(try await h.cache.pendingOutbox().last)
        let req = try JSONDecoder().decode(ProposeRequest.self, from: row.body ?? Data())
        #expect(req.ops.first?.kind == .replace(target: "b0", content: "abか"))
        tv.resignFirstResponder()
    }

    /// Changed on desktop while typed in: the conflict is saved flagged,
    /// and Take theirs drops our queued saves and shows their text.
    @Test func takeTheirsDropsOurQueuedSave() async throws {
        let h = try Harness(blocks("mine"))
        try await h.cache.storeDoc(DocTree(doc: DocSummary(id: "d", parentID: nil, title: "D", currentEpoch: 1),
                                           roots: h.model.session.editor.children(of: nil).map { BlockNode(block: $0) }))
        var remote = try #require(try await h.cache.editor(for: "d"))
        _ = try remote.apply(.replaceText("b0", "theirs"))
        h.type("b0", "!", at: 4)
        h.model.applyRemote(DocEditor(docID: "d", baseEpoch: 2, blocks: Array(remote.blocks.values)))
        #expect(h.model.session.item("b0")?.remoteText == "theirs")
        await h.model.flush()
        #expect(try await h.cache.conflictedBlocks("d") == ["b0": 1], "saved on its old epoch, flagged")
        await h.model.takeTheirs("b0")
        h.sync()
        #expect(h.markdown == ["theirs"])
        #expect(try await h.cache.pendingOutbox().isEmpty)
        #expect(try await h.cache.conflictedBlocks("d").isEmpty)
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
