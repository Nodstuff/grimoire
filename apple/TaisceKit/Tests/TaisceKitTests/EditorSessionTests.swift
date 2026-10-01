import Foundation
import Testing
@testable import TaisceKit

/// The op sequences behind the editor's structure changes.
@Suite(.timeLimit(.minutes(1))) struct EditorSessionTests {
    // fm (frontmatter, hidden), h1 { p1, p2 }, l1 (a list), c1 (code)
    func session() -> EditorSession {
        EditorSession(editor: DocEditor(docID: "d", baseEpoch: 4, blocks: [
            Block(id: "fm", docID: "d", parentID: nil, orderKey: "a", blockType: .code, content: "---\ntags: [x]\n---"),
            Block(id: "h1", docID: "d", parentID: nil, orderKey: "i", blockType: .heading, content: "# One"),
            Block(id: "p1", docID: "d", parentID: "h1", orderKey: "i", blockType: .paragraph, content: "alpha beta"),
            Block(id: "p2", docID: "d", parentID: "h1", orderKey: "r", blockType: .paragraph, content: "gamma"),
            Block(id: "l1", docID: "d", parentID: "h1", orderKey: "u", blockType: .paragraph, content: "- a\n- b"),
            Block(id: "c1", docID: "d", parentID: "h1", orderKey: "x", blockType: .code, content: "```\ncode\n```"),
        ]))
    }

    func p(_ s: String) -> EditorBlockContent { .paragraph(AttributedString(s)) }
    func ids(_ s: EditorSession) -> [BlockID] { s.items.map(\.id) }
    func kinds(_ r: ProposeRequest?) -> [String] {
        (r?.ops ?? []).map {
            switch $0.kind {
            case .insert: "insert"
            case .replace: "replace"
            case .delete: "delete"
            case .move: "move"
            }
        }
    }

    @Test func hiddenBlocksStayOut() {
        #expect(ids(session()) == ["h1", "p1", "p2", "l1", "c1"])
        #expect(session().item("c1")?.content.isRaw == true)
    }

    @Test func returnMidParagraphSplits() throws {
        var s = session()
        let changeResult = s.returnKey("p1", caret: EditorCaret(offset: 5))
        let change = try #require(changeResult)
        #expect(kinds(change.request) == ["replace", "insert"])
        let ops = try #require(change.request?.ops.map(\.kind))
        #expect(ops[0] == .replace(target: "p1", content: "alpha"))
        guard case let .insert(newID?, parent, key, _, content) = ops[1] else { Issue.record("an insert"); return }
        #expect(parent == "h1" && content == "beta" && key > "i" && key < "r", "between p1 and p2")
        #expect(change.focus == newID && change.caret == .start)
        #expect(ids(s) == ["h1", "p1", newID, "p2", "l1", "c1"])
        #expect(change.request?.baseEpoch == 4)
        #expect(s.commitText().isEmpty, "both halves are saved")
    }

    @Test func returnAtTheEndOfAHeadingStartsItsSection() throws {
        var s = session()
        let changeResult = s.returnKey("h1", caret: EditorCaret(offset: 3))
        let change = try #require(changeResult)
        #expect(kinds(change.request) == ["insert"])
        guard case let .insert(id?, parent, key, _, content)? = change.request?.ops.first?.kind else { Issue.record("an insert"); return }
        #expect(parent == "h1" && key < "i" && content == "", "the heading's first child")
        #expect(ids(s) == ["h1", id, "p1", "p2", "l1", "c1"])
    }

    @Test func returnAtTheStartInsertsAbove() throws {
        var s = session()
        let changeResult = s.returnKey("p2", caret: .start)
        let change = try #require(changeResult)
        guard case let .insert(id?, parent, key, _, _)? = change.request?.ops.first?.kind else { Issue.record("an insert"); return }
        #expect(parent == "h1" && key > "i" && key < "r")
        #expect(change.focus == "p2", "the caret stays with the text")
        #expect(ids(s) == ["h1", "p1", id, "p2", "l1", "c1"])
    }

    @Test func returnInAListIsATextChange() throws {
        var s = session()
        let changeResult = s.returnKey("l1", caret: EditorCaret(line: 1, offset: 1))
        let change = try #require(changeResult)
        #expect(change.request == nil && change.reload == ["l1"])
        #expect(change.caret == EditorCaret(line: 2, offset: 0))
        #expect(s.commitText().first?.ops.map(\.kind) == [.replace(target: "l1", content: "- a\n- b\n- ")])
        // Return on that empty last item: leave the list
        let leaveResult = s.returnKey("l1", caret: EditorCaret(line: 2, offset: 0))
        let leave = try #require(leaveResult)
        #expect(kinds(leave.request) == ["replace", "insert"])
        #expect(leave.request?.ops.first?.kind == .replace(target: "l1", content: "- a\n- b"))
    }

    @Test func backspaceMergesIntoThePreviousBlock() throws {
        var s = session()
        let changeResult = s.backspaceAtStart("p2", caret: .start)
        let change = try #require(changeResult)
        #expect(change.request?.ops.map(\.kind) == [.replace(target: "p1", content: "alpha betagamma"), .delete(target: "p2")])
        #expect(change.focus == "p1" && change.caret == EditorCaret(offset: 10))
        #expect(ids(s) == ["h1", "p1", "l1", "c1"])
    }

    @Test func mergingAHeadingKeepsItsSection() throws {
        var s = session()
        // the heading first turns into text...
        let demoteResult = s.backspaceAtStart("h1", caret: .start)
        let demote = try #require(demoteResult)
        #expect(demote.request == nil && s.item("h1")?.content == p("One"))
        // ...and a heading with children merged away hands them to its parent
        var t = EditorSession(editor: DocEditor(docID: "d", baseEpoch: 1, blocks: [
            Block(id: "a", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "intro"),
            Block(id: "h", docID: "d", parentID: nil, orderKey: "r", blockType: .heading, content: "# H"),
            Block(id: "k1", docID: "d", parentID: "h", orderKey: "i", blockType: .paragraph, content: "k1"),
            Block(id: "k2", docID: "d", parentID: "h", orderKey: "r", blockType: .paragraph, content: "k2"),
        ]))
        t.update("h", content: p("H"))
        let mergeResult = t.backspaceAtStart("h", caret: .start)
        let merge = try #require(mergeResult)
        #expect(kinds(merge.request) == ["replace", "move", "move", "delete"])
        #expect(merge.request?.ops.first?.kind == .replace(target: "a", content: "introH"))
        #expect(t.editor.ordered().map(\.block.id) == ["a", "k1", "k2"])
        #expect(t.editor.ordered().allSatisfy { $0.block.parentID == nil })
    }

    @Test func backspaceIntoRawSourceJustMoves() throws {
        var s = EditorSession(editor: DocEditor(docID: "d", baseEpoch: 1, blocks: [
            Block(id: "c", docID: "d", parentID: nil, orderKey: "i", blockType: .code, content: "```\ncode\n```"),
            Block(id: "e", docID: "d", parentID: nil, orderKey: "r", blockType: .paragraph, content: ""),
            Block(id: "t", docID: "d", parentID: nil, orderKey: "u", blockType: .paragraph, content: "text"),
        ]))
        let goneResult = s.backspaceAtStart("e", caret: .start)
        let gone = try #require(goneResult)
        #expect(gone.focus == "c" && gone.caret == EditorCaret(offset: 12))
        #expect(gone.request?.ops.map(\.kind) == [.delete(target: "e")], "the empty block goes; the code is untouched")
        let moveResult = s.backspaceAtStart("t", caret: .start)
        let move = try #require(moveResult)
        #expect(move.focus == "c" && move.request == nil, "text never merges into source")
        #expect(s.items.map(\.id) == ["c", "t"])
    }

    @Test func listItemToParagraphSplitsTheBlock() throws {
        var s = EditorSession(editor: DocEditor(docID: "d", baseEpoch: 1, blocks: [
            Block(id: "l", docID: "d", parentID: nil, orderKey: "i", blockType: .paragraph, content: "- a\n- b\n- c"),
        ]))
        let changeResult = s.backspaceAtStart("l", caret: EditorCaret(line: 1, offset: 0))
        let change = try #require(changeResult)
        #expect(kinds(change.request) == ["replace", "insert", "insert"])
        let contents = s.items.map(\.content.markdown)
        #expect(contents == ["- a", "b", "- c"])
        #expect(s.editor.ordered().map(\.block.content) == ["- a", "b", "- c"])
        #expect(change.focus == s.items[1].id)
    }

    @Test func emptyDocGetsADraftThatInsertsOnSave() throws {
        var s = EditorSession(editor: DocEditor(docID: "d", baseEpoch: 0, blocks: [
            Block(id: "fm", docID: "d", parentID: nil, orderKey: "i", blockType: .code, content: "---\na: b\n---"),
        ]))
        #expect(s.items.count == 1 && s.items[0].isDraft)
        #expect(s.commitText().isEmpty == false, "a draft always saves")
        var t = EditorSession(editor: DocEditor(docID: "d", baseEpoch: 0, blocks: [
            Block(id: "fm", docID: "d", parentID: nil, orderKey: "i", blockType: .code, content: "---\na: b\n---"),
        ]))
        let id = t.items[0].id
        t.update(id, content: p("first words"))
        let reqResult = t.commitText().first
        let req = try #require(reqResult)
        guard case let .insert(bid, parent, key, _, content) = req.ops[0].kind else { Issue.record("an insert"); return }
        #expect(bid == id && parent == nil && key > "i" && content == "first words", "after the frontmatter")
        #expect(t.commitText().isEmpty)
        // Return in a draft: the draft is inserted first, then the new block after it
        var u = EditorSession(editor: DocEditor(docID: "d", baseEpoch: 0, blocks: []))
        let d = u.items[0].id
        u.update(d, content: p("ab"))
        let changeResult = u.returnKey(d, caret: EditorCaret(offset: 2))
        let change = try #require(changeResult)
        #expect(kinds(change.request) == ["insert", "insert"])
        #expect(u.editor.ordered().map(\.block.id) == [d, change.focus])
    }

    @Test func remoteChangesNeverYankTheFocusedBlock() throws {
        var s = session()
        s.update("p1", content: p("alpha beta, typing"))
        var remote = s.editor
        _ = try remote.apply(.replaceText("p1", "alpha (their edit)"))
        _ = try remote.apply(.replaceText("p2", "gamma (their edit)"))
        let newer = DocEditor(docID: "d", baseEpoch: 7, blocks: Array(remote.blocks.values))
        let reload = s.refresh(newer, keep: ["p1"])
        #expect(reload == ["p2"])
        #expect(s.item("p1")?.content == p("alpha beta, typing"), "the block under the caret keeps its text")
        #expect(s.item("p2")?.content == p("gamma (their edit)"))
        // the save of the conflicted block goes against the older epoch, alone
        let reqs = s.commitText()
        #expect(reqs.count == 1 && reqs[0].baseEpoch == 4)
        #expect(reqs[0].ops.map(\.kind) == [.replace(target: "p1", content: "alpha beta, typing")])
        // later saves are on the new epoch
        s.update("p2", content: p("gamma 2"))
        #expect(s.commitText().first?.baseEpoch == 7)
    }

    @Test func aBlockDeletedRemotelyWhileTypedInBecomesADraft() throws {
        var s = session()
        s.update("p2", content: p("gamma, still typing"))
        var remote = s.editor
        _ = try remote.apply(.delete("p2"))
        s.refresh(DocEditor(docID: "d", baseEpoch: 9, blocks: Array(remote.blocks.values)), keep: ["p2"])
        #expect(ids(s) == ["h1", "p1", "p2", "l1", "c1"])
        #expect(s.item("p2")?.isDraft == true)
        let reqResult = s.commitText().first
        let req = try #require(reqResult)
        guard case let .insert(id, parent, _, _, content) = req.ops[0].kind else { Issue.record("an insert"); return }
        #expect(id == "p2" && parent == "h1" && content == "gamma, still typing")
    }

    @Test func refreshAddsAndDropsUnheldBlocks() throws {
        var s = session()
        var remote = s.editor
        _ = try remote.apply(.delete("p2"))
        _ = try remote.apply(.insert(after: "l1", parent: "h1", type: .paragraph, content: "new from desktop", id: "n"))
        let reload = s.refresh(DocEditor(docID: "d", baseEpoch: 6, blocks: Array(remote.blocks.values)), keep: [])
        #expect(ids(s) == ["h1", "p1", "l1", "n", "c1"])
        #expect(reload == ["n"])
    }
}
