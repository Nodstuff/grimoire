import Foundation
import Testing
import TaisceKit
@testable import Taisce

private let mine = Workspace(id: "w1", name: "Work", sortKey: "a", role: "owner")
private let shared = Workspace(id: "w2", name: "Family", sortKey: "b", role: "editor", ownerName: "Aoife", shared: true)
private let viewed = Workspace(id: "w3", name: "Club", sortKey: "c", role: "viewer", ownerName: "Ciara", shared: true)

/// w1: proj (spec, notes (deep)), loose; w2: fam (recipes); w3: club; Unsorted: inbox
private let docs = [
    DocInfo(id: "proj", title: "Project", sortKey: "a", workspaceID: "w1"),
    DocInfo(id: "spec", parentID: "proj", title: "Spec", sortKey: "a", workspaceID: "w1"),
    DocInfo(id: "notes", parentID: "proj", title: "Notes", sortKey: "i", workspaceID: "w1"),
    DocInfo(id: "deep", parentID: "notes", title: "Deep", sortKey: "a", workspaceID: "w1"),
    DocInfo(id: "loose", title: "Loose", sortKey: "i", workspaceID: "w1"),
    DocInfo(id: "fam", title: "Family", sortKey: "r", workspaceID: "w2"),
    DocInfo(id: "recipes", parentID: "fam", title: "Recipes", sortKey: "a", workspaceID: "w2"),
    DocInfo(id: "club", title: "Club", sortKey: "s", workspaceID: "w3"),
    DocInfo(id: "inbox", title: "Inbox", sortKey: "t"),
]

private let index = DocIndex(docs)
private let placement = DocMoves.placement(docs)
private func access(_ id: DocID) -> EditAccess {
    EditAccess.forDoc(workspaceID: index.byID[id]?.workspaceID, workspaces: [mine, shared, viewed])
}

@Suite struct DocMoveRuleTests {
    @Test func reparentingFollowsTheRole() {
        #expect(access("proj").canReparent, "owner")
        #expect(access("recipes").canReparent, "editor")
        #expect(!access("club").canReparent, "viewer")
        #expect(access("inbox").canReparent, "your Unsorted")
        #expect(EditAccess.forDoc(workspaceID: "unknown", workspaces: []).canReparent, "LOCAL / not loaded: the server decides")
        #expect(!EditAccess.viewOnly.canReparent && EditAccess.full.canReparent)
    }

    @Test func dropIntoAnotherDocInTheSameWorkspace() {
        #expect(DocMoves.canDrop("loose", onto: "spec", index: index, placement: placement, access: access))
        #expect(DocMoves.canDrop("deep", onto: "loose", index: index, placement: placement, access: access))
        #expect(DocMoves.canDrop("recipes", onto: "fam", index: index, placement: placement, access: access), "an editor reorders within")
    }

    @Test func neverIntoItselfOrBelowItself() {
        #expect(!DocMoves.canDrop("proj", onto: "proj", index: index, placement: placement, access: access))
        #expect(!DocMoves.canDrop("proj", onto: "notes", index: index, placement: placement, access: access))
        #expect(!DocMoves.canDrop("proj", onto: "deep", index: index, placement: placement, access: access))
    }

    @Test func neverAcrossWorkspacesOrInAViewersOne() {
        #expect(!DocMoves.canDrop("loose", onto: "fam", index: index, placement: placement, access: access))
        #expect(!DocMoves.canDrop("inbox", onto: "proj", index: index, placement: placement, access: access))
        #expect(!DocMoves.canDrop("club", onto: "club", index: index, placement: placement, access: access))
        #expect(!DocMoves.canDrop("missing", onto: "proj", index: index, placement: placement, access: access))
    }

    @Test func theTopLevelIsAnOwnersCall() {
        #expect(DocMoves.canDrop("spec", onto: nil, index: index, placement: placement, access: access))
        #expect(!DocMoves.canDrop("loose", onto: nil, index: index, placement: placement, access: access), "already there")
        #expect(!DocMoves.canDrop("recipes", onto: nil, index: index, placement: placement, access: access), "an editor can't re-home it")
    }

    @Test func thePickerOffersTheWorkspaceMinusTheSubtree() {
        let options = DocMoves.options(moving: "notes", index: index, access: access)
        #expect(options.map(\.parent) == [nil, "loose", "proj", "spec"])
        #expect(options.first?.title == "Top level")
        #expect(options.first { $0.parent == "proj" }?.isCurrent == true)
        #expect(options.first { $0.parent == "spec" }?.breadcrumb == "Project")
        // search matches titles and paths, ignoring case and accents
        #expect(DocMoves.options(moving: "notes", index: index, access: access, query: "SPÉC").map(\.parent) == ["spec"])
        #expect(DocMoves.options(moving: "loose", index: index, access: access, query: "project").map(\.parent) == ["proj", "notes", "deep", "spec"])
    }

    @Test func anEditorsPickerHasNoTopLevel() {
        let options = DocMoves.options(moving: "recipes", index: index, access: access)
        #expect(options.map(\.parent) == ["fam"])
        #expect(options.first?.isCurrent == true)
    }

    @Test func aDragCarriesOnlyTaggedIDs() {
        #expect(DocDrag(id: "d1").token == "taisce-doc:d1")
        #expect(DocDrag(token: "taisce-doc:d1")?.id == "d1")
        #expect(DocDrag(token: "some text") == nil)
        #expect(DocDrag(token: "taisce-doc:") == nil)
    }
}
