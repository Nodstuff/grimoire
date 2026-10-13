import Foundation
import TaisceKit

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

    /// Whether `dragged` may be dropped onto `target` (nil = the top level).
    static func canDrop(_ dragged: DocID, onto target: DocID?, index: DocIndex, placement: DocPlacement, access: (DocID) -> EditAccess) -> Bool {
        guard let doc = index.byID[dragged], access(dragged).canReparent else { return false }
        guard let target else {
            // already there; otherwise only an owner re-homes to the top
            return doc.parentID != nil && access(dragged).canMove
        }
        guard let into = index.byID[target], into.workspaceID == doc.workspaceID, access(target).canReparent else { return false }
        return placement.canPlace(dragged, under: target)
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

/// What a drag carries: a doc id, as tagged text so plain text dragged in
/// from elsewhere is refused (no custom UTType, so no Info.plist change).
struct DocDrag: Hashable, Sendable {
    static let prefix = "taisce-doc:"
    var id: DocID

    var token: String { Self.prefix + id }

    init(id: DocID) { self.id = id }

    init?(token: String) {
        guard token.hasPrefix(Self.prefix), token.count > Self.prefix.count else { return nil }
        id = String(token.dropFirst(Self.prefix.count))
    }
}

extension AppModel {
    /// May this doc be dragged or "Move to…"'d? (Not in a workspace you only view.)
    func canReparent(_ id: DocID) -> Bool {
        index.byID[id] != nil && access(for: id).canReparent
    }

    func canDrop(_ dragged: DocID, onto target: DocID?) -> Bool {
        DocMoves.canDrop(dragged, onto: target, index: index, placement: DocMoves.placement(docs), access: access(for:))
    }

    func moveOptions(for id: DocID, query: String = "") -> [DocMoveOption] {
        DocMoves.options(moving: id, index: index, access: access(for:), query: query)
    }

    /// A drop: false (refused at once) when it can't go there.
    @discardableResult
    func dropDoc(_ dragged: DocID, onto target: DocID?) -> Bool {
        guard canDrop(dragged, onto: target) else { return false }
        Task { await moveDoc(dragged, into: target) }
        return true
    }

    /// Make `id` the last child of `parent` (nil = the top level of its
    /// workspace) through the outbox: Library shows it at once, the write
    /// lands when online, a refusal puts it back and sets `moveError`.
    func moveDoc(_ id: DocID, into parent: DocID?) async {
        guard let cache, let doc = index.byID[id] else { return }
        let key = DocMoves.placement(docs).appendKey(moving: id, under: parent)
        // at the top level an unlabelled doc would leave its workspace
        let keep = parent == nil && hasWorkspaces ? doc.workspaceID : nil
        do {
            try await cache.enqueueDocMove(id, parent: parent, sortKey: key, keepWorkspace: keep)
        } catch {
            moveError = "Couldn't move it: \(error.localizedDescription)"
            lastError = moveError
            return
        }
        await replayOutbox()
    }
}
