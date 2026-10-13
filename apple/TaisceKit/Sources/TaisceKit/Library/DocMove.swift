import Foundation
import GRDB

// Moving a doc in the tree (a new parent, a new place among its siblings):
// `POST /api/doc/{id}/move` `{parent_id, sort_key}` (crates/daemon/src/api.rs
// `move_doc`). The server checks write access on the doc and the new parent
// and refuses a cycle. A null parent is the root; a null sort_key appends.

extension APIClient {
    /// Move `id` under `parent` (nil = top level) at `sortKey` (nil = last).
    public func moveDoc(_ id: DocID, parent: DocID?, sortKey: String?) async throws {
        struct OK: Decodable {}
        let _: OK = try await post("/api/doc/\(id)/move", body: MoveDocBody(parentID: parent, sortKey: sortKey))
    }

    /// `parent_id` is always sent (`null` = the root).
    struct MoveDocBody: Codable, Equatable {
        var parentID: DocID?
        var sortKey: String?

        enum CodingKeys: String, CodingKey {
            case parentID = "parent_id"
            case sortKey = "sort_key"
        }

        init(parentID: DocID?, sortKey: String?) {
            self.parentID = parentID
            self.sortKey = sortKey
        }

        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(parentID, forKey: .parentID)
            try c.encode(sortKey, forKey: .sortKey)
        }
    }
}

/// Where a doc may go, and the sort key that puts it there. Pure: built
/// from the tree rows the client holds.
public struct DocPlacement: Sendable {
    public struct Row: Sendable, Hashable {
        public var id: DocID
        public var parentID: DocID?
        public var sortKey: String?

        public init(id: DocID, parentID: DocID?, sortKey: String?) {
            self.id = id
            self.parentID = parentID
            self.sortKey = sortKey
        }
    }

    let parent: [DocID: DocID]
    /// siblings in the server's order: by sort_key, unkeyed last
    let children: [DocID?: [Row]]

    public init(_ rows: [Row]) {
        var parent: [DocID: DocID] = [:]
        for r in rows { if let p = r.parentID { parent[r.id] = p } }
        self.parent = parent
        var children: [DocID?: [Row]] = [:]
        for r in rows { children[r.parentID, default: []].append(r) }
        for k in children.keys {
            children[k] = children[k]?.enumerated().sorted { a, b in
                switch (a.element.sortKey, b.element.sortKey) {
                case let (x?, y?) where x != y: x < y
                case (_?, nil): true
                case (nil, _?): false
                default: a.offset < b.offset
                }
            }.map(\.element)
        }
        self.children = children
    }

    public init(_ records: [DocRecord]) {
        self.init(records.map { Row(id: $0.id, parentID: $0.parentID, sortKey: $0.sortKey) })
    }

    /// Whether `candidate` is `id` or somewhere below it (cycle-safe).
    public func isSelfOrDescendant(_ candidate: DocID, of id: DocID) -> Bool {
        var seen: Set<DocID> = []
        var next: DocID? = candidate
        while let c = next, seen.insert(c).inserted {
            if c == id { return true }
            next = parent[c]
        }
        return false
    }

    /// Whether `id` may go under `newParent` (nil = top level): never into
    /// itself or its own subtree (the server refuses the cycle too).
    public func canPlace(_ id: DocID, under newParent: DocID?) -> Bool {
        guard let newParent else { return true }
        return !isSelfOrDescendant(newParent, of: id)
    }

    /// The key that makes `id` the last child of `newParent`.
    public func appendKey(moving id: DocID, under newParent: DocID?) -> String {
        let siblings = (children[newParent] ?? []).filter { $0.id != id }
        return Self.keyForPosition(siblings.map(\.sortKey), index: siblings.count)
    }

    public enum Side: Sendable { case before, after }

    /// The parent and key that put `id` just before or after `target`
    /// among `target`'s siblings; nil when that would be a cycle.
    public func place(_ id: DocID, _ side: Side, _ target: DocID) -> (parent: DocID?, sortKey: String)? {
        guard id != target else { return nil }
        let newParent = parent[target]
        guard canPlace(id, under: newParent) else { return nil }
        let siblings = (children[newParent] ?? []).filter { $0.id != id }
        guard let i = siblings.firstIndex(where: { $0.id == target }) else { return nil }
        return (newParent, Self.keyForPosition(siblings.map(\.sortKey), index: side == .before ? i : i + 1))
    }

    /// Key for a doc dropped at `index` of `siblings` (the moved doc
    /// already removed). Unkeyed siblings sort last on the server and bound
    /// nothing, so the bounds are the nearest keyed neighbours on each side
    /// (ui/src/editor/diff.ts `keyForPosition`).
    public static func keyForPosition(_ siblings: [String?], index: Int) -> String {
        var lo: String?
        var i = min(index, siblings.count) - 1
        while i >= 0 {
            if let k = siblings[i] { lo = k; break }
            i -= 1
        }
        var hi: String?
        var j = max(index, 0)
        while j < siblings.count {
            if let k = siblings[j] { hi = k; break }
            j += 1
        }
        return OrderKey.between(lo, hi)
    }
}

/// A queued move, read back from its outbox row.
public struct QueuedDocMove: Sendable, Hashable {
    public var doc: DocID
    public var parent: DocID?
    public var sortKey: String?
}

extension Cache {
    /// Queue a move (survives offline) and show it at once: the cached row
    /// takes its new parent and key now, and keeps them through tree
    /// refreshes until the write lands or is refused (then the replayer puts
    /// the server's placement back).
    ///
    /// `keepWorkspace`: a move to the top level would drop an inherited
    /// workspace (the server files an unlabelled root in the mover's
    /// Unsorted), so the doc is labelled with it first, as its own queued
    /// write; the move waits on that label (`labelKey`).
    @discardableResult
    public func enqueueDocMove(_ id: DocID, parent: DocID?, sortKey: String?, keepWorkspace: WorkspaceID? = nil, key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        let body = try JSONEncoder().encode(APIClient.MoveDocBody(parentID: parent, sortKey: sortKey))
        let label: Data? = try (parent == nil ? keepWorkspace : nil).map {
            try JSONEncoder().encode(APIClient.DocWorkspaceBody(workspaceID: $0, requestID: Self.labelKey(forMove: key)))
        }
        return try await db.write { db in
            if let existing = try OutboxEntry.filter(OutboxEntry.Columns.idempotencyKey == key).fetchOne(db) {
                return existing
            }
            if let label {
                var l = OutboxEntry(
                    id: nil, createdAt: now, idempotencyKey: Self.labelKey(forMove: key), method: "PUT",
                    path: "/api/docs/\(id)/workspace", body: label, state: .pending, attempts: 0, lastError: nil
                )
                try l.insert(db)
            }
            var e = OutboxEntry(
                id: nil, createdAt: now, idempotencyKey: key, method: "POST",
                path: "/api/doc/\(id)/move", body: body, state: .pending, attempts: 0, lastError: nil
            )
            try e.insert(db)
            try Self.place(db, id, parent: parent, sortKey: sortKey)
            return e
        }
    }

    /// The label write a move to the top level waits on.
    public static func labelKey(forMove key: String) -> String { "\(key):label" }

    public func outboxEntry(key: String) async throws -> OutboxEntry? {
        try await db.read { db in try OutboxEntry.filter(OutboxEntry.Columns.idempotencyKey == key).fetchOne(db) }
    }

    /// Set a cached doc's place (the server's, after a refused move), then
    /// lay any still-queued moves back on top.
    public func setDocPlacement(_ id: DocID, parent: DocID?, sortKey: String?) async throws {
        try await db.write { db in
            try Self.place(db, id, parent: parent, sortKey: sortKey)
            try Self.reapplyQueuedMoves(db)
        }
    }

    /// Moves not yet landed or refused, oldest first.
    public func queuedMoves() async throws -> [QueuedDocMove] {
        try await db.read { db in try Self.queuedMoves(db) }
    }

    /// `POST /api/doc/{id}/move`'s target, or nil for any other write.
    public static func docMove(of e: OutboxEntry) -> QueuedDocMove? {
        let parts = e.path.split(separator: "/")
        guard e.method == "POST", parts.count == 4, parts[0] == "api", parts[1] == "doc", parts[3] == "move",
              let body = e.body, let b = try? JSONDecoder().decode(APIClient.MoveDocBody.self, from: body)
        else { return nil }
        return QueuedDocMove(doc: String(parts[2]), parent: b.parentID, sortKey: b.sortKey)
    }

    static func place(_ db: Database, _ id: DocID, parent: DocID?, sortKey: String?) throws {
        try db.execute(sql: "UPDATE docs SET parent_id = ?, sort_key = ? WHERE id = ?", arguments: [parent, sortKey, id])
    }

    static func queuedMoves(_ db: Database) throws -> [QueuedDocMove] {
        try OutboxEntry
            .filter([OutboxEntry.State.pending.rawValue, OutboxEntry.State.inflight.rawValue].contains(OutboxEntry.Columns.state))
            .filter(OutboxEntry.Columns.path.like("/api/doc/%/move"))
            .order(OutboxEntry.Columns.id)
            .fetchAll(db)
            .compactMap(docMove)
    }

    /// The server's tree rows don't know about our queued moves yet: keep
    /// showing them (the newest per doc wins, as it will on the server).
    static func reapplyQueuedMoves(_ db: Database) throws {
        for m in try queuedMoves(db) { try place(db, m.doc, parent: m.parent, sortKey: m.sortKey) }
    }
}

extension OutboxReplayer {
    /// The words for a refused move, from the server's answer.
    public static func moveRefusal(_ e: APIError) -> String {
        let why: String
        switch e {
        case let .server(msg), let .notFound(msg): why = msg
        case let .http(status): why = "the server answered \(status)"
        default: why = String(describing: e)
        }
        if why.contains("nest under itself") { return "Couldn't move it: a doc can't go inside itself." }
        if case .notFound = e { return "Couldn't move it: that doc or folder no longer exists." }
        var text = why
        for prefix in ["forbidden: ", "invalid op: ", "move: "] where text.hasPrefix(prefix) { text.removeFirst(prefix.count) }
        return "Couldn't move it: \(text)"
    }

    /// A move was refused: put the doc back where the server has it (or
    /// drop it, if it's gone). Best effort: offline, the next tree refresh
    /// settles it (the refused row no longer counts as queued).
    func rollBackMove(_ doc: DocID) async {
        do {
            let tree = try await api.doc(doc)
            try await cache.setDocPlacement(doc, parent: tree.doc.parentID, sortKey: tree.doc.sortKey)
        } catch APIError.notFound {
            try? await cache.deleteDoc(doc)
        } catch {}
    }
}
