import Foundation
import TaisceKit

/// A tree row as the UI needs it, independent of the cache record.
struct DocInfo: Identifiable, Hashable, Sendable {
    var id: DocID
    var parentID: DocID?
    var title: String
    var sortKey: String?
    /// resolved workspace (nil = Unsorted)
    var workspaceID: WorkspaceID?

    init(id: DocID, parentID: DocID? = nil, title: String, sortKey: String? = nil, workspaceID: WorkspaceID? = nil) {
        self.id = id
        self.parentID = parentID
        self.title = title
        self.sortKey = sortKey
        self.workspaceID = workspaceID
    }

    init(_ r: DocRecord) {
        self.init(id: r.id, parentID: r.parentID, title: r.title, sortKey: r.sortKey, workspaceID: r.workspaceID)
    }
}

/// Lookups over the flat tree: ancestors, breadcrumbs, wikilink targets.
struct DocIndex: Sendable {
    static let separator = " › "

    let docs: [DocInfo]
    let byID: [DocID: DocInfo]
    let childCount: [DocID: Int]

    init(_ docs: [DocInfo]) {
        self.docs = docs
        byID = Dictionary(docs.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        var counts: [DocID: Int] = [:]
        for d in docs { if let p = d.parentID { counts[p, default: 0] += 1 } }
        childCount = counts
    }

    /// Ancestors, root first, not including the doc itself. Stops on a
    /// missing parent or a cycle.
    func ancestors(of id: DocID) -> [DocInfo] {
        var out: [DocInfo] = []
        var seen: Set<DocID> = [id]
        var next = byID[id]?.parentID
        while let p = next, !seen.contains(p), let doc = byID[p] {
            out.insert(doc, at: 0)
            seen.insert(p)
            next = doc.parentID
        }
        return out
    }

    /// "Grimoire › iOS app" — where the doc lives; nil at the root.
    func breadcrumb(of id: DocID) -> String? {
        let path = ancestors(of: id)
        return path.isEmpty ? nil : path.map(\.title).joined(separator: Self.separator)
    }

    /// The immediate folder's title.
    func folder(of id: DocID) -> String? { ancestors(of: id).last?.title }

    /// Wikilinks name docs by title, optionally with a parent path
    /// (`[[Grimoire/Roadmap]]`): an exact path match wins, then the leaf title.
    func doc(titled target: String) -> DocInfo? {
        let parts = target.split(separator: "/").map { $0.trimmingCharacters(in: .whitespaces) }
        guard let leaf = parts.last else { return nil }
        let candidates = docs.filter { $0.title.caseInsensitiveCompare(leaf) == .orderedSame }
        if parts.count > 1 {
            let wanted = parts.dropLast().map { $0.lowercased() }
            if let hit = candidates.first(where: { d in
                let path = ancestors(of: d.id).map { $0.title.lowercased() }
                return path.suffix(wanted.count).elementsEqual(wanted)
            }) { return hit }
        }
        return candidates.first(where: { $0.title == leaf }) ?? candidates.first
    }
}

/// A Library row: the doc, its subtitle, and its children (nil = leaf).
struct LibraryNode: Identifiable, Hashable, Sendable {
    var doc: DocInfo
    var subtitle: String?
    var children: [LibraryNode]?
    var id: DocID { doc.id }
    var isFolder: Bool { children != nil }

    static func build(_ docs: [DocInfo]) -> [LibraryNode] {
        let index = DocIndex(docs)
        // a doc whose parent we don't have (trashed, not shared to us) shows at the root
        let byParent = Dictionary(grouping: docs) { d in d.parentID.flatMap { index.byID[$0] == nil ? nil : $0 } }
        let order: (DocInfo, DocInfo) -> Bool = { ($0.sortKey ?? "", $0.title) < ($1.sortKey ?? "", $1.title) }
        var seen: Set<DocID> = []
        func node(_ d: DocInfo) -> LibraryNode {
            seen.insert(d.id)
            let kids = (byParent[d.id] ?? []).filter { !seen.contains($0.id) }.sorted(by: order).map(node)
            return LibraryNode(doc: d, subtitle: subtitle(d, kids: kids.count, index: index), children: kids.isEmpty ? nil : kids)
        }
        var roots = (byParent[nil] ?? []).sorted(by: order).map(node)
        // a parent cycle (a → b → a) never reaches the root: show it there
        for d in docs.sorted(by: order) where !seen.contains(d.id) {
            roots.append(node(d))
        }
        return roots
    }

    /// The folders above `id` in `nodes`, outermost first; nil when the
    /// tree doesn't hold it (another workspace's doc).
    static func ancestors(of id: DocID, in nodes: [LibraryNode]) -> [DocID]? {
        for n in nodes {
            if n.id == id { return [] }
            if let kids = n.children, let below = ancestors(of: id, in: kids) { return [n.id] + below }
        }
        return nil
    }

    /// Folders say how much is inside; nested leaves say where they live.
    static func subtitle(_ d: DocInfo, kids: Int, index: DocIndex) -> String? {
        if kids > 0 { return kids == 1 ? "1 doc" : "\(kids) docs" }
        return index.breadcrumb(of: d.id)
    }
}

/// Who last wrote a doc, and when (from its op ledger).
struct EditMeta: Hashable, Sendable {
    var author: String
    var date: Date

    /// The newest applied op; nil when the ledger has no dated row.
    init?(history: [DocHistoryEntry]) {
        guard let row = history.first(where: { $0.applied && $0.date != nil }), let date = row.date else { return nil }
        author = row.principalName.isEmpty ? "someone" : row.principalName
        self.date = date
    }

    init(author: String, date: Date) {
        self.author = author
        self.date = date
    }
}

/// A pinned doc on Today.
struct DocCardModel: Identifiable, Hashable, Sendable {
    var id: DocID
    var title: String
    var folder: String?
    var edited: Date?

    /// "Folder · 2 h ago", with either half omitted when unknown.
    func subtitle(now: Date = .now) -> String? {
        let parts = [folder, edited.map { RelativeTime.string($0, now: now) }].compactMap { $0 }
        return parts.isEmpty ? nil : parts.joined(separator: " · ")
    }

    /// Pinned ids in pin order, dropping pins whose doc is gone.
    static func pinned(_ pins: [DocID], index: DocIndex, meta: [DocID: EditMeta]) -> [DocCardModel] {
        pins.compactMap { id in
            index.byID[id].map { DocCardModel(id: id, title: $0.title, folder: index.folder(of: id), edited: meta[id]?.date) }
        }
    }
}

enum RelativeTime {
    /// "just now", "5 min ago", "2 h ago", "yesterday", "3 d ago", then a date.
    static func string(_ date: Date, now: Date = .now, calendar: Calendar = .current) -> String {
        let s = now.timeIntervalSince(date)
        if s < 60 { return "just now" }
        if s < 3600 { return "\(Int(s / 60)) min ago" }
        if calendar.isDate(date, inSameDayAs: now) || s < 6 * 3600 { return "\(Int(s / 3600)) h ago" }
        let days = calendar.dateComponents([.day], from: calendar.startOfDay(for: date), to: calendar.startOfDay(for: now)).day ?? 0
        if days == 1 { return "yesterday" }
        if days < 7 { return "\(days) d ago" }
        let f = DateFormatter()
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.setLocalizedDateFormatFromTemplate(calendar.isDate(date, equalTo: now, toGranularity: .year) ? "d MMM" : "d MMM yyyy")
        return f.string(from: date)
    }

    /// "Wednesday 1 October"
    static func dayLine(_ date: Date = .now, calendar: Calendar = .current) -> String {
        let f = DateFormatter()
        f.calendar = calendar
        f.timeZone = calendar.timeZone
        f.setLocalizedDateFormatFromTemplate("EEEE d MMMM")
        return f.string(from: date).replacingOccurrences(of: ",", with: "")
    }
}
