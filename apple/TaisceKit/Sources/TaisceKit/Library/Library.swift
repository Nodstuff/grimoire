import Foundation

/// The flat doc list as a tree for `OutlineGroup` (nil children = leaf).
public struct DocTreeNode: Identifiable, Hashable, Sendable {
    public var doc: DocRecord
    public var children: [DocTreeNode]?
    public var id: DocID { doc.id }

    public static func build(_ docs: [DocRecord]) -> [DocTreeNode] {
        // a doc whose parent we don't have (trashed, not shared to us) shows at the root
        let ids = Set(docs.map(\.id))
        let byParent = Dictionary(grouping: docs) { $0.parentID.flatMap { ids.contains($0) ? $0 : nil } }
        let order: (DocRecord, DocRecord) -> Bool = { ($0.sortKey ?? "", $0.title) < ($1.sortKey ?? "", $1.title) }
        var seen: Set<DocID> = []
        func node(_ d: DocRecord) -> DocTreeNode {
            seen.insert(d.id)
            let kids = (byParent[d.id] ?? []).filter { !seen.contains($0.id) }.sorted(by: order).map(node)
            return DocTreeNode(doc: d, children: kids.isEmpty ? nil : kids)
        }
        var roots = (byParent[nil] ?? []).sorted(by: order).map(node)
        // a parent cycle (a → b → a) never reaches the root: show it there
        for d in docs.sorted(by: order) where !seen.contains(d.id) {
            roots.append(node(d))
        }
        return roots
    }
}

/// Lookups the views make over the cached doc list.
public enum Library {
    /// Wikilinks name docs by title, optionally with a parent path.
    public static func doc(titled title: String, in docs: [DocRecord]) -> DocRecord? {
        let leaf = title.split(separator: "/").last.map(String.init) ?? title
        return docs.first { $0.title == title } ?? docs.first { $0.title == leaf }
    }

    public static func todoDocID(in docs: [DocRecord]) -> DocID? {
        docs.first { $0.parentID == nil && $0.title == TodoParser.todoDocTitle }?.id
    }
}
