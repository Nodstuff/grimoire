import Foundation
import TaisceKit
import UniformTypeIdentifiers

/// One row of the "Move to…" picker: a doc to go under, or the top level.
struct DocMoveOption: Identifiable, Hashable, Sendable {
    /// nil = the top level
    var parent: DocID?
    var title: String
    /// where that doc lives ("Grimoire › iOS app"); nil at the root
    var breadcrumb: String?
    /// the doc is already here (offered, but a no-op)
    var isCurrent: Bool

    var id: String { parent ?? "" }
}

/// Moving a doc in the tree, the pure rules. A doc moves within its own
/// workspace only (a workspace change is "Move to workspace…"), never into
/// itself or below itself, and only where you may write.
enum DocMoves {
    static func placement(_ docs: [DocInfo]) -> DocPlacement {
        DocPlacement(docs.map { DocPlacement.Row(id: $0.id, parentID: $0.parentID, sortKey: $0.sortKey) })
    }

    /// Why `dragged` can't go onto `target` (nil = the top level); nil when it can.
    static func refusal(_ dragged: DocID, onto target: DocID?, index: DocIndex, placement: DocPlacement, access: (DocID) -> EditAccess) -> String? {
        guard let doc = index.byID[dragged] else { return "That doc isn't here any more." }
        guard access(dragged).canReparent else { return "You can only view this workspace, so its docs stay where they are." }
        guard let target else {
            if doc.parentID == nil { return "It's already at the top level." }
            // the server files it in your Unsorted, and only the owner re-homes
            return access(dragged).canMove ? nil : "Only the workspace's owner can move docs to its top level."
        }
        guard let into = index.byID[target] else { return "That doc isn't here any more." }
        guard into.workspaceID == doc.workspaceID else { return "Docs move within their workspace; use Move to workspace\u{2026} first." }
        guard access(target).canReparent else { return "You can only view that workspace." }
        guard placement.canPlace(dragged, under: target) else {
            return dragged == target ? "A doc can't go inside itself." : "A doc can't go inside one of its own docs."
        }
        return nil
    }

    /// Whether `dragged` may be dropped onto `target` (nil = the top level).
    static func canDrop(_ dragged: DocID, onto target: DocID?, index: DocIndex, placement: DocPlacement, access: (DocID) -> EditAccess) -> Bool {
        refusal(dragged, onto: target, index: index, placement: placement, access: access) == nil
    }

    /// Where `dragged` lands just before or after `target` (its parent and
    /// key), or why it can't (why nil and no spot: a no-op, onto itself).
    /// Among its own siblings it is a reorder; elsewhere the move rules hold.
    static func place(_ dragged: DocID, _ side: DocPlacement.Side, _ target: DocID, index: DocIndex, placement: DocPlacement,
                      access: (DocID) -> EditAccess) -> (spot: (parent: DocID?, sortKey: String)?, why: String?) {
        guard let doc = index.byID[dragged] else { return (nil, "That doc isn't here any more.") }
        guard dragged != target else { return (nil, nil) }
        guard let t = index.byID[target] else { return (nil, "That doc isn't here any more.") }
        guard t.workspaceID == doc.workspaceID else { return (nil, "Docs move within their workspace; use Move to workspace\u{2026} first.") }
        // a doc with no order key (an old one) sorts last and bounds nothing:
        // beside it the doc would land somewhere else, so only inside it
        guard t.sortKey != nil else { return (nil, "That doc has no place in the order yet; drop onto it instead.") }
        guard let spot = placement.place(dragged, side, target) else { return (nil, "A doc can't go inside one of its own docs.") }
        if spot.parent == doc.parentID {
            return access(dragged).canReparent ? (spot, nil) : (nil, "You can only view this workspace, so its docs stay where they are.")
        }
        if let why = refusal(dragged, onto: spot.parent, index: index, placement: placement, access: access) { return (nil, why) }
        return (spot, nil)
    }

    /// A move to the top level labels the doc with its workspace first, but
    /// only when that workspace comes from its parent: a doc with its own
    /// label carries it anyway, and the label's undo after a refusal must
    /// leave the doc resolving where it was (its parent's workspace).
    static func labelForTopLevel(_ id: DocID, index: DocIndex) -> WorkspaceID? {
        guard let doc = index.byID[id], let w = doc.workspaceID,
              let parent = doc.parentID.flatMap({ index.byID[$0] }), parent.workspaceID == w
        else { return nil }
        return w
    }

    /// Every place `id` can go, the top level first, then docs in path
    /// order; `query` narrows by title or path (case and accents ignored).
    static func options(moving id: DocID, index: DocIndex, access: (DocID) -> EditAccess, query: String = "") -> [DocMoveOption] {
        guard let doc = index.byID[id] else { return [] }
        let placement = placement(index.docs)
        let q = query.trimmingCharacters(in: .whitespaces)
        func matches(_ s: String) -> Bool {
            q.isEmpty || s.range(of: q, options: [.caseInsensitive, .diacriticInsensitive]) != nil
        }
        var out: [DocMoveOption] = []
        if access(id).canMove, matches("Top level") {
            out.append(DocMoveOption(parent: nil, title: "Top level", breadcrumb: nil, isCurrent: doc.parentID == nil))
        }
        let candidates = index.docs
            .filter { $0.workspaceID == doc.workspaceID && placement.canPlace(id, under: $0.id) && access($0.id).canReparent }
            .map { d -> (DocInfo, String?, String) in
                let crumb = index.breadcrumb(of: d.id)
                return (d, crumb, [crumb, d.title].compactMap { $0 }.joined(separator: DocIndex.separator))
            }
            .filter { matches($0.2) }
            .sorted { ($0.2.localizedLowercase, $0.0.id) < ($1.2.localizedLowercase, $1.0.id) }
        out += candidates.map { DocMoveOption(parent: $0.0.id, title: $0.0.title, breadcrumb: $0.1, isCurrent: $0.0.id == doc.parentID) }
        return out
    }
}

extension UTType {
    /// A dragged Library row (declared in Info.plist's
    /// UTExportedTypeDeclarations). Data, not text: the editor and other
    /// apps don't take it, and only Library rows accept it.
    static let taisceDoc = UTType(exportedAs: "ie.null.taisce.doc-reference", conformingTo: .data)
}

/// What a drag carries: the doc id under `UTType.taisceDoc`, visible to this
/// app only.
enum DocDrag {
    static func provider(_ id: DocID) -> NSItemProvider {
        let p = NSItemProvider()
        let data = Data(id.utf8)
        p.registerDataRepresentation(forTypeIdentifier: UTType.taisceDoc.identifier, visibility: .ownProcess) { done in
            done(data, nil)
            return nil
        }
        return p
    }
}

extension AppModel {
    /// May this doc be dragged or "Move to…"'d? (Not in a workspace you only view.)
    func canReparent(_ id: DocID) -> Bool {
        index.byID[id] != nil && access(for: id).canReparent
    }

    func moveRefusal(_ dragged: DocID, onto target: DocID?) -> String? {
        DocMoves.refusal(dragged, onto: target, index: index, placement: DocMoves.placement(docs), access: access(for:))
    }

    func canDrop(_ dragged: DocID, onto target: DocID?) -> Bool {
        moveRefusal(dragged, onto: target) == nil
    }

    func moveOptions(for id: DocID, query: String = "") -> [DocMoveOption] {
        DocMoves.options(moving: id, index: index, access: access(for:), query: query)
    }

    /// A drop. One that can't go there says why (the hover already showed
    /// it refused where SwiftUI lets us); false then.
    @discardableResult
    func dropDoc(_ dragged: DocID, onto target: DocID?) -> Bool {
        draggingDoc = nil
        if let why = moveRefusal(dragged, onto: target) {
            moveError = "Couldn't move it: \(why)"
            return false
        }
        Task { await moveDoc(dragged, into: target) }
        return true
    }

    /// Where `dragged` lands just before or after `target`, or why it can't.
    func placeRefusal(_ dragged: DocID, _ side: DocPlacement.Side, _ target: DocID) -> (spot: (parent: DocID?, sortKey: String)?, why: String?) {
        DocMoves.place(dragged, side, target, index: index, placement: DocMoves.placement(docs), access: access(for:))
    }

    func canPlace(_ dragged: DocID, _ side: DocPlacement.Side, _ target: DocID) -> Bool {
        placeRefusal(dragged, side, target).spot != nil
    }

    /// A drop just before or after a row: its sibling there.
    @discardableResult
    func placeDoc(_ dragged: DocID, _ side: DocPlacement.Side, _ target: DocID) -> Bool {
        draggingDoc = nil
        let r = placeRefusal(dragged, side, target)
        guard let spot = r.spot else {
            if let why = r.why { moveError = "Couldn't move it: \(why)" }
            return false
        }
        Task { await moveDoc(dragged, into: spot.parent, sortKey: spot.sortKey) }
        return true
    }

    /// Make `id` the last child of `parent` (nil = the top level of its
    /// workspace), or put it at `sortKey` there, through the outbox:
    /// Library shows it at once (an append's key the server picks when it
    /// lands); a refusal puts it back and sets `moveError`.
    func moveDoc(_ id: DocID, into parent: DocID?, sortKey: String? = nil) async {
        guard let cache, index.byID[id] != nil else { return }
        // at the top level an inherited workspace would be lost
        let keep = parent == nil && hasWorkspaces ? DocMoves.labelForTopLevel(id, index: index) : nil
        let key = UUID().uuidString.lowercased()
        movesThisSession.insert(key)
        do {
            try await cache.enqueueDocMove(id, parent: parent, sortKey: sortKey, keepWorkspace: keep, key: key)
        } catch {
            moveError = "Couldn't move it: \(error.localizedDescription)"
            return
        }
        await replayOutbox()
    }
}
