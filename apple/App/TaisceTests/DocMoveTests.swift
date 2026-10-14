import Foundation
import Testing
import TaisceKit
import UniformTypeIdentifiers
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

    /// Dropped on a row's top or bottom edge: its sibling, just before or after.
    @Test func besideARowIsItsSibling() throws {
        func place(_ d: DocID, _ side: DocPlacement.Side, _ t: DocID) -> (spot: (parent: DocID?, sortKey: String)?, why: String?) {
            DocMoves.place(d, side, t, index: index, placement: placement, access: access)
        }
        // a reorder among siblings: notes before spec (keys a, i → before a)
        let r = try #require(place("notes", .before, "spec").spot)
        #expect(r.parent == "proj" && r.sortKey < "a")
        // out of a folder to the top level, after proj (an owner's call: w1 is mine)
        let out = try #require(place("spec", .after, "proj").spot)
        #expect(out.parent == nil && out.sortKey > "a" && out.sortKey < "i")
        // into another folder's children, beside one of them
        let into = try #require(place("loose", .after, "deep").spot)
        #expect(into.parent == "notes" && into.sortKey > "a")
        // never below itself, never across workspaces, never in a viewer's
        #expect(place("proj", .after, "deep").spot == nil)
        #expect(place("loose", .before, "fam").spot == nil)
        #expect(place("club", .before, "club").spot == nil && place("club", .before, "club").why == nil, "onto itself: nothing")
        #expect(place("inbox", .before, "proj").spot == nil, "Unsorted and w1 are different spaces")
        // an editor reorders within a shared workspace, but the top level is the owner's
        #expect(place("recipes", .before, "fam").spot == nil, "w2's top level is Aoife's")
        // beside a doc with no order key: nowhere (only inside it)
        let unkeyed = docs + [DocInfo(id: "old", title: "Old", workspaceID: "w1")]
        let ui = DocIndex(unkeyed)
        #expect(DocMoves.place("loose", .before, "old", index: ui, placement: DocMoves.placement(unkeyed), access: access).spot == nil)
    }

    @Test func theDropZoneFollowsThePointer() {
        #expect(DocDropZone.at(2, height: 30) == .before)
        #expect(DocDropZone.at(15, height: 30) == .into)
        #expect(DocDropZone.at(28, height: 30) == .after)
        #expect(DocDropZone.at(10, height: 0) == .into)
    }

    /// An empty, never-written doc (New folder) is a folder before anything is in it.
    @Test func anEmptyDocIsAFolder() {
        let nodes = LibraryNode.build([
            DocInfo(id: "box", title: "Box", sortKey: "a", currentEpoch: 0),
            DocInfo(id: "page", title: "Page", sortKey: "b", currentEpoch: 3),
        ])
        #expect(nodes[0].isFolder && nodes[0].children == nil)
        #expect(!nodes[1].isFolder)
        let open = DocTreeLine.lines(nodes, isOpen: { _ in true })
        #expect(open.map(\.isOpen) == [true, false], "an empty folder opens; a page doesn't")
    }

    @Test func theTreeShowsOpenFoldersOnly() {
        let nodes = LibraryNode.build(docs)
        let closed = DocTreeLine.lines(nodes, isOpen: { _ in false }).map(\.id)
        #expect(closed == ["proj", "loose", "fam", "club", "inbox"])
        let open = DocTreeLine.lines(nodes, isOpen: { $0 == "proj" || $0 == "notes" })
        #expect(open.map(\.id) == ["proj", "spec", "notes", "deep", "loose", "fam", "club", "inbox"])
        #expect(open.map(\.depth) == [0, 1, 1, 2, 0, 0, 0, 0])
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

    /// Not text: the editor (and other apps) never take a dragged row, and
    /// rows take nothing else. The type is declared by the app's Info.plist.
    @Test func aDragCarriesOnlyTheAppsOwnType() async throws {
        #expect(UTType.taisceDoc.isDeclared && UTType.taisceDoc.conforms(to: .data))
        #expect(!UTType.taisceDoc.conforms(to: .text))
        let p = DocDrag.provider("d1")
        #expect(p.registeredTypeIdentifiers == ["ie.null.taisce.doc-reference"])
        #expect(!p.hasItemConformingToTypeIdentifier(UTType.plainText.identifier))
        let data: Data = try await withCheckedThrowingContinuation { c in
            _ = p.loadDataRepresentation(forTypeIdentifier: UTType.taisceDoc.identifier) { d, e in
                if let d { c.resume(returning: d) } else { c.resume(throwing: e ?? CocoaError(.fileReadUnknown)) }
            }
        }
        #expect(String(decoding: data, as: UTF8.self) == "d1")
    }

    @Test func aRefusedDropSaysWhy() {
        func why(_ d: DocID, _ t: DocID?) -> String? { DocMoves.refusal(d, onto: t, index: index, placement: placement, access: access) }
        #expect(why("proj", "proj") == "A doc can't go inside itself.")
        #expect(why("proj", "deep") == "A doc can't go inside one of its own docs.")
        #expect(why("loose", nil) == "It's already at the top level.")
        #expect(why("recipes", nil) == "Only the workspace's owner can move docs to its top level.")
        #expect(why("club", "club")?.hasPrefix("You can only view") == true)
        #expect(why("loose", "fam")?.contains("Move to workspace") == true)
        #expect(why("loose", "spec") == nil)
    }

    /// Only a doc whose workspace comes from its parent is labelled on the
    /// way to the top (and so only such a label is ever undone).
    @Test func onlyAnInheritedWorkspaceIsLabelledOnTheWayUp() {
        #expect(DocMoves.labelForTopLevel("spec", index: index) == "w1")
        #expect(DocMoves.labelForTopLevel("loose", index: index) == nil, "a root carries its own")
        #expect(DocMoves.labelForTopLevel("inbox", index: index) == nil, "Unsorted")
        let away = DocIndex(docs + [DocInfo(id: "own", parentID: "proj", title: "Own", workspaceID: "w2")])
        #expect(DocMoves.labelForTopLevel("own", index: away) == nil, "labelled away from its parent: its own label travels")
    }
}
