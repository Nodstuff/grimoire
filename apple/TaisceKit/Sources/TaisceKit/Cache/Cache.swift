import Foundation
import GRDB

/// The offline cache: doc tree, doc bodies (blocks, FTS5-indexed), to-dos
/// parsed from the To-do doc, the sync cursor and the write outbox.
public final class Cache: Sendable {
    let db: any DatabaseWriter

    /// On-disk cache (WAL). Pass a path under Application Support.
    public convenience init(path: String) throws {
        try Cache.protectDirectory(ofDatabaseAt: path)
        try self.init(writer: DatabasePool(path: path))
        try Cache.protectFiles(ofDatabaseAt: path)
    }

    /// In-memory cache, for tests and previews.
    public static func inMemory() throws -> Cache {
        try Cache(writer: DatabaseQueue())
    }

    init(writer: any DatabaseWriter) throws {
        db = writer
        try Self.migrator.migrate(db)
    }

    static var migrator: DatabaseMigrator {
        var m = DatabaseMigrator()
        m.registerMigration("v1") { db in
            try db.create(table: "docs") { t in
                t.primaryKey("id", .text)
                t.column("parent_id", .text).indexed()
                t.column("title", .text).notNull()
                t.column("current_epoch", .integer).notNull()
                t.column("sort_key", .text)
                t.column("status", .text)
                t.column("is_canvas", .boolean).notNull().defaults(to: false)
                t.column("is_shared", .boolean).notNull().defaults(to: false)
                t.column("mirror_permission", .text)
                t.column("body_epoch", .integer)
            }
            try db.create(table: "blocks") { t in
                t.primaryKey("id", .text)
                t.column("doc_id", .text).notNull().indexed()
                    .references("docs", onDelete: .cascade)
                t.column("parent_id", .text)
                t.column("order_key", .text).notNull()
                t.column("block_type", .text).notNull()
                t.column("content", .text).notNull()
                t.column("epoch", .integer).notNull()
                t.column("refers_to", .text)
                t.column("position", .integer).notNull()
                t.column("depth", .integer).notNull()
            }
            try db.create(virtualTable: "blocks_fts", using: FTS5()) { t in
                t.synchronize(withTable: "blocks")
                t.tokenizer = .unicode61()
                t.column("content")
            }
            try db.create(table: "todos") { t in
                t.column("date", .text).notNull()
                t.column("position", .integer).notNull()
                t.column("mark", .text).notNull()
                t.column("text", .text).notNull()
                t.column("deadline", .text)
                t.column("carried_from", .text)
                t.column("note", .text)
                t.primaryKey(["date", "position"])
            }
            try db.create(table: "sync_state") { t in
                t.primaryKey("key", .text)
                t.column("value", .integer).notNull()
            }
            try db.create(table: "outbox") { t in
                t.autoIncrementedPrimaryKey("id")
                t.column("created_at", .datetime).notNull()
                t.column("idempotency_key", .text).notNull().unique()
                t.column("method", .text).notNull()
                t.column("path", .text).notNull()
                t.column("body", .blob)
                t.column("state", .text).notNull().indexed()
                t.column("attempts", .integer).notNull().defaults(to: 0)
                t.column("last_error", .text)
            }
        }
        m.registerMigration("v2") { db in
            // a landed propose's answer (`ProposeOutcome`): its verdicts drive
            // the editor's review marks, its epoch chains later proposes
            try db.alter(table: "outbox") { t in t.add(column: "outcome", .blob) }
        }
        m.registerMigration("v2-workspaces") { db in
            try db.alter(table: "docs") { t in t.add(column: "workspace_id", .text) }
            try db.create(table: "workspaces") { t in
                t.primaryKey("id", .text)
                t.column("name", .text).notNull()
                t.column("color", .text)
                t.column("icon", .text)
                t.column("sort_key", .text)
                t.column("doc_count", .integer).notNull().defaults(to: 0)
            }
            // one list per workspace: rows are keyed by their To-do doc too
            try db.drop(table: "todos")
            try db.create(table: "todos") { t in
                t.column("doc_id", .text).notNull()
                t.column("date", .text).notNull()
                t.column("position", .integer).notNull()
                t.column("mark", .text).notNull()
                t.column("text", .text).notNull()
                t.column("deadline", .text)
                t.column("carried_from", .text)
                t.column("note", .text)
                t.primaryKey(["doc_id", "date", "position"])
            }
            // the dropped rows come back on the next fetch of each To-do doc
            try db.execute(sql: "UPDATE docs SET body_epoch = NULL WHERE title = ?", arguments: [TodoParser.todoDocTitle])
            // re-bootstrap (bodies kept) so every doc row gains its workspace_id
            try db.execute(sql: "DELETE FROM sync_state WHERE key = 'last_seq'")
        }
        return m
    }

    // MARK: sync cursor

    public func lastSeq() async throws -> Int {
        try await db.read { db in
            try Int.fetchOne(db, sql: "SELECT value FROM sync_state WHERE key = 'last_seq'") ?? 0
        }
    }

    /// Advance the cursor. Never moves it backwards (a late writer from a
    /// stopped loop, a confused server); `resetLastSeq` is the one way back.
    public func setLastSeq(_ seq: Int) async throws {
        try await db.write { db in
            try db.execute(
                sql: "INSERT INTO sync_state(key, value) VALUES ('last_seq', ?) ON CONFLICT(key) DO UPDATE SET value = MAX(value, excluded.value)",
                arguments: [seq]
            )
        }
    }

    /// Put the cursor anywhere, backwards included: the server was reset.
    public func resetLastSeq(_ seq: Int) async throws {
        try await db.write { db in
            try db.execute(
                sql: "INSERT INTO sync_state(key, value) VALUES ('last_seq', ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                arguments: [seq]
            )
        }
    }

    /// Every cached body needs a refetch before it is trusted again.
    public func markAllBodiesStale() async throws {
        try await db.write { db in
            try db.execute(sql: "UPDATE docs SET body_epoch = -1 WHERE body_epoch IS NOT NULL")
        }
    }

    // MARK: tree

    /// Replace the tree with the server's list, keeping cached bodies (and
    /// their epochs) for docs that still exist.
    public func replaceTree(_ summaries: [DocSummary]) async throws {
        try await db.write { db in
            let bodyEpochs = try Dictionary(
                Row.fetchAll(db, sql: "SELECT id, body_epoch FROM docs").map { ($0["id"] as String, $0["body_epoch"] as Int?) },
                uniquingKeysWith: { a, _ in a }
            )
            let keep = Set(summaries.map(\.id))
            for id in bodyEpochs.keys where !keep.contains(id) {
                _ = try DocRecord.deleteOne(db, key: id)
            }
            for s in summaries {
                try DocRecord(s, bodyEpoch: bodyEpochs[s.id] ?? nil).upsert(db)
            }
        }
    }

    /// Apply one doc's state from a change row, keeping its cached body and
    /// tree decorations. A `deleted` state removes the doc.
    public func applyDocState(_ id: DocID, _ s: Change.DocState) async throws {
        try await db.write { db in
            if s.deleted {
                _ = try DocRecord.deleteOne(db, key: id)
                return
            }
            var rec = try DocRecord.fetchOne(db, key: id)
                ?? DocRecord(DocSummary(id: id, parentID: s.parentID, title: s.title, currentEpoch: s.currentEpoch), bodyEpoch: nil)
            rec.title = s.title
            rec.parentID = s.parentID
            rec.sortKey = s.sortKey
            rec.status = s.status
            rec.workspaceID = s.workspaceID
            rec.currentEpoch = max(rec.currentEpoch, s.currentEpoch)
            try rec.upsert(db)
        }
    }

    public func docs() async throws -> [DocRecord] {
        try await db.read { db in try DocRecord.order(DocRecord.Columns.sortKey, DocRecord.Columns.title).fetchAll(db) }
    }

    public func doc(_ id: DocID) async throws -> DocRecord? {
        try await db.read { db in try DocRecord.fetchOne(db, key: id) }
    }

    /// Mark a doc's body stale without fetching it (a change for a doc we
    /// never opened).
    public func noteEpoch(_ id: DocID, epoch: Int) async throws {
        try await db.write { db in
            try db.execute(sql: "UPDATE docs SET current_epoch = MAX(current_epoch, ?) WHERE id = ?", arguments: [epoch, id])
        }
    }

    // MARK: bodies

    /// Upsert the doc row and replace its blocks with the fetched tree.
    public func storeDoc(_ tree: DocTree) async throws {
        let flat = tree.flattened().filter { !$0.block.deleted }
        // every workspace has its own To-do doc, at any depth
        let todos = tree.doc.title == TodoParser.todoDocTitle
            ? TodoParser.parse(markdown: flat.map(\.block.content).joined(separator: "\n\n")).map { var t = $0; t.docID = tree.doc.id; return t }
            : nil
        try await db.write { db in
            var rec = DocRecord(tree.doc, bodyEpoch: tree.doc.currentEpoch)
            if let existing = try DocRecord.fetchOne(db, key: tree.doc.id) {
                // /api/doc/{id}'s `doc` lacks the tree decorations; keep ours
                rec.isCanvas = existing.isCanvas
                rec.isShared = existing.isShared
                rec.mirrorPermission = existing.mirrorPermission
                rec.workspaceID = existing.workspaceID
            }
            try rec.upsert(db)
            try BlockRecord.filter(BlockRecord.Columns.docID == tree.doc.id).deleteAll(db)
            for (i, item) in flat.enumerated() {
                let b = item.block
                try BlockRecord(
                    id: b.id, docID: b.docID, parentID: b.parentID, orderKey: b.orderKey,
                    blockType: b.blockType.rawValue, content: b.content, epoch: b.epoch,
                    refersTo: b.refersTo, position: i, depth: item.depth
                ).upsert(db)
            }
            if let todos {
                try TodoRecord.filter(TodoRecord.Columns.docID == tree.doc.id).deleteAll(db)
                for t in todos { try t.upsert(db) }
            }
        }
    }

    public func blocks(of id: DocID) async throws -> [BlockRecord] {
        try await db.read { db in
            try BlockRecord.filter(BlockRecord.Columns.docID == id).order(BlockRecord.Columns.position).fetchAll(db)
        }
    }

    public func deleteDoc(_ id: DocID) async throws {
        try await db.write { db in _ = try DocRecord.deleteOne(db, key: id) }
    }

    /// Offline full-text search over cached bodies.
    public func searchBlocks(_ query: String, limit: Int = 20) async throws -> [BlockRecord] {
        guard let pattern = FTS5Pattern(matchingAllPrefixesIn: query) else { return [] }
        return try await db.read { db in
            try BlockRecord.fetchAll(db, sql: """
                SELECT blocks.* FROM blocks
                JOIN blocks_fts ON blocks_fts.rowid = blocks.rowid AND blocks_fts MATCH ?
                ORDER BY rank LIMIT ?
                """, arguments: [pattern, limit])
        }
    }

    // MARK: to-dos

    /// Every cached list's items (every workspace: what alerts plan from).
    public func todos() async throws -> [TodoRecord] {
        try await db.read { db in try TodoRecord.order(TodoRecord.Columns.date, TodoRecord.Columns.position).fetchAll(db) }
    }

    /// One To-do doc's items.
    public func todos(in docID: DocID) async throws -> [TodoRecord] {
        try await db.read { db in
            try TodoRecord.filter(TodoRecord.Columns.docID == docID).order(TodoRecord.Columns.date, TodoRecord.Columns.position).fetchAll(db)
        }
    }

    // MARK: workspaces

    public func replaceWorkspaces(_ all: [Workspace]) async throws {
        try await db.write { db in
            try WorkspaceRecord.deleteAll(db)
            for w in all { try WorkspaceRecord(w).insert(db) }
        }
    }

    public func workspaces() async throws -> [Workspace] {
        try await db.read { db in try WorkspaceRecord.fetchAll(db).map(\.workspace) }
    }

    // MARK: observation (for the UI)

    /// The To-do doc's items, re-emitted whenever sync rewrites them.
    public func observeTodos() -> AsyncValueObservation<[TodoRecord]> {
        ValueObservation.tracking { db in
            try TodoRecord.order(TodoRecord.Columns.date, TodoRecord.Columns.position).fetchAll(db)
        }.values(in: db)
    }

    public func observeTree() -> AsyncValueObservation<[DocRecord]> {
        ValueObservation
            .tracking { db in try DocRecord.order(DocRecord.Columns.sortKey, DocRecord.Columns.title).fetchAll(db) }
            .values(in: db)
    }

    public func observeBlocks(of id: DocID) -> AsyncValueObservation<[BlockRecord]> {
        ValueObservation
            .tracking { db in
                try BlockRecord.filter(BlockRecord.Columns.docID == id).order(BlockRecord.Columns.position).fetchAll(db)
            }
            // any doc's write touches the blocks table: emit only real changes to this one
            .removeDuplicates()
            .values(in: db)
    }
}
