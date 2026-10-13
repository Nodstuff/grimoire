import Foundation
import Testing
@testable import TaisceKit

/// Text blocks holding several elements (a line then a list; lists split by
/// blank lines) show as ordinary text and save back as the one block.
@Suite struct CompoundTests {
    static let kiran = "Questions for Kiran\n* Is it ok to now consider it defunct?\n* I am inverting the permissions model"
    static let shortlist = "- Andrew Stewart (rockets)\n- Ibrahima (cybersecurity)\n\n\n- Nicholas (side projects)\n- Ray (motivated)\n\n\n- Nina (motivated)"

    func doc(_ content: String) -> DocEditor {
        DocEditor(docID: "d", baseEpoch: 1, blocks: [
            Block(id: "fm", docID: "d", parentID: nil, orderKey: "a", blockType: .code, content: "---\ntags:\n  - inbox\n---"),
            Block(id: "x", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: content),
            Block(id: "z", docID: "d", parentID: nil, orderKey: "r", blockType: .paragraph, content: "after"),
        ])
    }

    /// What the app's save does: fold the surface into the server's blocks.
    func ops(_ s: NSAttributedString, _ baseline: [DocDiff.BaselineBlock], _ editor: DocEditor) -> [BlockOp] {
        DocDiff.computeOps(baseline: Compound.fold(baseline),
                           entries: Compound.fold(Surface.entries(s, known: Set(baseline.map(\.id)))),
                           hidden: Surface.hidden(editor))
    }

    @Test func partsOfMixedBlocks() {
        #expect(Compound.parts(Self.kiran) == ["Questions for Kiran", "* Is it ok to now consider it defunct?\n* I am inverting the permissions model"])
        #expect(Compound.parts(Self.shortlist)?.count == 3)
        // not compound: one element, code, a heading part, plain prose
        #expect(Compound.parts("- a\n- b") == nil)
        #expect(Compound.parts("text\n```\ncode\n```") == nil)
        #expect(Compound.parts("# Title\nbody") == ["# Title", "body"])
        #expect(Compound.parts("| a |\n|---|\n| 1 |") == nil)
        #expect(Compound.parts("one paragraph\nsoft broken") == nil)
    }

    @Test(arguments: [kiran, shortlist])
    func showsAsTextAndUntouchedSavesNothing(_ content: String) {
        let editor = doc(content)
        let (blocks, baseline) = Surface.load(editor)
        #expect(!blocks.contains { $0.content.isRaw }, "no source block: \(blocks.map(\.content))")
        #expect(blocks.first?.id == "x")
        let s = Surface.document(blocks)
        #expect(!s.string.contains("\u{FFFC}"))
        #expect(ops(s, baseline, editor).isEmpty)
        // the same ids every time (a refresh splits the same way)
        #expect(Surface.load(editor).blocks.map(\.id) == blocks.map(\.id))
    }

    @Test func anEditInAPartIsOneReplaceOfTheBlock() {
        let editor = doc(Self.kiran)
        let (blocks, baseline) = Surface.load(editor)
        let s = Surface.document(blocks)
        // type at the end of the list's last item
        let end = (s.string as NSString).range(of: "permissions model").upperBound
        s.replaceCharacters(in: NSRange(location: end, length: 0),
                            with: NSAttributedString(string: "!", attributes: Surface.typingAttributes(s, at: end)))
        var sel = NSRange(location: end + 1, length: 0)
        Surface.normalize(s, selection: &sel)
        let o = ops(s, baseline, editor)
        #expect(o == [.replace(target: "x", content: "Questions for Kiran\n\n* Is it ok to now consider it defunct?\n* I am inverting the permissions model!")])
        // after the save the baseline is in parts again: nothing more to send
        let entries = Surface.entries(s, known: Set(baseline.map(\.id)))
        let folded = Compound.fold(baseline)
        let rebuilt = Compound.unfold(DocDiff.rebuildBaseline(folded, entries: Compound.fold(entries), ops: o), entries: entries)
        #expect(rebuilt.map(\.id) == blocks.map(\.id))
        #expect(ops(s, rebuilt, editor).isEmpty)
    }

    @Test func aPartMovedAwayBecomesItsOwnBlock() {
        let editor = doc(Self.kiran)
        let (blocks, baseline) = Surface.load(editor)
        // a new paragraph typed between the line and the list
        var shown = blocks
        shown.insert((id: "new", content: .paragraph(AttributedString("between"))), at: 1)
        let s = Surface.document(shown)
        let o = ops(s, baseline, editor)
        #expect(o.contains(.replace(target: "x", content: "Questions for Kiran")))
        #expect(o.contains { if case let .insert(_, _, _, _, content) = $0 { content == "between" } else { false } })
        #expect(o.contains { if case let .insert(id, _, _, _, content) = $0 { id == blocks[1].id && content.hasPrefix("* Is it ok") } else { false } })
    }

    /// Plain text that used to open as a source block: a heading starting
    /// with a number, list items wrapped onto indented lines.
    @Test(arguments: ["## 1. LRU cache", "### 2) Pointer-mode `x` — first",
                      "- **a**: one\n  two\n- b", "- [ ] `x` emits\n      only\n- [ ] y", "1. **W** one\n   two\n2. B"])
    func everydayTextIsText(_ md: String) {
        #expect(!EditorBlockContent.parse(markdown: md).isRaw, "\(md)")
    }
}
