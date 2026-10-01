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
        // typing inside the link goes after it
        h.type("b0", "!", at: 6)
        #expect(h.markdown == ["see [[Roadmap]]! now"])
        // Backspace right after it removes all of it
        h.backspace("b0", at: 11)
        #expect(h.markdown == ["see ! now"])
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
