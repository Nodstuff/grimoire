import Foundation
import GRDB

/// Agent Fabric turns, per session, so a session opens at once with what was
/// last seen and fetches only what is newer (`…/turns?after=`). A turn never
/// changes once it has ended, so this is append-only but for two things: a
/// fuller copy of the same turn replaces a shorter one (the live event over a
/// Text off route's blank), and each session keeps only its newest
/// `fabricTurnsKept`. It lives in the doc cache's database so whatever forgets
/// the person's docs forgets these too: `wipe` (sign-out, another person
/// signing in, `CacheOwnership`), and one file per server.
///
/// Text off: a turn marked `textOff` is stored without its prompt and reply,
/// whatever copy arrives (the live event carries them; the route never does).
///
/// Contiguity: the stream's turns are written as they arrive, but the stream
/// misses whatever ends while the app is away or the stream is down, so the
/// newest turn held says nothing about what is held before it. Each session
/// keeps a watermark instead, `synced`: the newest `ended` up to which what
/// is held is known to be every turn the server has (only a read of the
/// routes moves it: a newest page, or an `after=` page, never a stream
/// turn). A top-up asks `after=` from the watermark, so a hole the stream
/// left is fetched and filled, and the stream's turns above it are merged
/// by id. A session cached before the watermark existed has none: the
/// top-up starts from its oldest held turn, once.
extension Cache {
    /// how many of a session's newest turns are kept
    public static let fabricTurnsKept = 100

    /// One session's cached turns, oldest first, and whether the server has
    /// turns older than the oldest of them (true when not known).
    public struct CachedTurns: Sendable, Hashable {
        public var turns: [FabricTurn]
        public var older: Bool
        /// what is held is every turn the server has up to this `ended`
        /// (nil: not known; a top-up starts from the oldest held)
        public var synced: Int?
    }

    static func registerFabricTurns(_ m: inout DatabaseMigrator) {
        m.registerMigration("v5-fabric-turns") { db in
            try db.create(table: "fabric_turns") { t in
                t.column("runtime_id", .text).notNull()
                t.column("session_id", .text).notNull()
                t.column("turn_id", .text).notNull()
                t.column("ended", .integer).notNull()
                t.column("started", .integer).notNull()
                t.column("reply_length", .integer).notNull()
                // the turn as JSON (`FabricTurn`'s own coding) (`FabricTurn`'s own coding)
                t.column("body", .blob).notNull()
                t.primaryKey(["runtime_id", "session_id", "turn_id"])
            }
            try db.create(index: "fabric_turns_by_end", on: "fabric_turns", columns: ["runtime_id", "session_id", "ended"])
            // per session: whether the server holds turns older than the oldest cached
            try db.create(table: "fabric_turn_sessions") { t in
                t.column("runtime_id", .text).notNull()
                t.column("session_id", .text).notNull()
                t.column("older", .boolean).notNull()
                t.primaryKey(["runtime_id", "session_id"])
            }
        }
        m.registerMigration("v6-fabric-synced") { db in
            // the contiguity watermark (see the type's comment); NULL for
            // what v5 wrote, which may hold a hole the stream left
            try db.alter(table: "fabric_turn_sessions") { t in t.add(column: "synced", .integer) }
        }
    }

    /// What is held for one session.
    public func fabricTurns(_ key: FleetSessionKey) async throws -> CachedTurns {
        try await db.read { db in
            let bodies = try Data.fetchAll(db, sql: """
                SELECT body FROM fabric_turns WHERE runtime_id = ? AND session_id = ? ORDER BY ended, started
                """, arguments: [key.runtimeID, key.id])
            let decoder = JSONDecoder()
            let turns = bodies.compactMap { try? decoder.decode(FabricTurn.self, from: $0) }
            let row = try Row.fetchOne(db, sql: "SELECT older, synced FROM fabric_turn_sessions WHERE runtime_id = ? AND session_id = ?",
                                       arguments: [key.runtimeID, key.id])
            let older: Bool = row?["older"] ?? true
            let synced: Int? = row?["synced"]
            return CachedTurns(turns: turns, older: older, synced: turns.isEmpty ? nil : synced)
        }
    }

    /// Keep these turns (any sessions). A turn already held keeps the copy
    /// with the longer reply. `olderBeyond`: the server's `more` for a page
    /// read newest-first (`before=`), recorded when that page reaches as far
    /// back as anything held. `syncedThrough`: these came from a route read
    /// that leaves what is held gap-free up to this `ended` (the watermark
    /// only moves forward; the stream's turns never pass one). A session
    /// past `keep` turns loses its oldest.
    public func cacheFabricTurns(_ turns: [FabricTurn], olderBeyond: Bool? = nil, syncedThrough: Int? = nil,
                                 keep: Int = Cache.fabricTurnsKept) async throws {
        guard !turns.isEmpty else { return }
        let encoder = JSONEncoder()
        let rows = try turns.filter { !$0.turnID.isEmpty }.map { t -> (FabricTurn, Data) in
            let stored = Self.storable(t)
            return (stored, try encoder.encode(stored))
        }
        try await db.write { db in
            for (key, group) in Dictionary(grouping: rows, by: { $0.0.session }) {
                let oldest = try Int.fetchOne(db, sql: "SELECT MIN(ended) FROM fabric_turns WHERE runtime_id = ? AND session_id = ?",
                                              arguments: [key.runtimeID, key.id])
                for (t, body) in group {
                    try db.execute(sql: """
                        INSERT INTO fabric_turns(runtime_id, session_id, turn_id, ended, started, reply_length, body)
                        VALUES (?, ?, ?, ?, ?, ?, ?)
                        ON CONFLICT(runtime_id, session_id, turn_id) DO UPDATE SET
                          ended = excluded.ended, started = excluded.started, reply_length = excluded.reply_length, body = excluded.body
                        WHERE excluded.reply_length >= fabric_turns.reply_length OR ? = 1
                        """, arguments: [t.runtimeID, t.sessionID, t.turnID, t.ended, t.started, t.reply.count, body, t.textOff])
                }
                if let olderBeyond, let pageOldest = group.map(\.0.ended).min(), oldest.map({ pageOldest <= $0 }) ?? true {
                    try Self.setOlder(db, key, olderBeyond)
                }
                if let syncedThrough { try Self.setSynced(db, key, syncedThrough) }
                try Self.trim(db, key, keep: keep)
            }
        }
    }

    /// Hold exactly these for one session (an old server's newest page that
    /// does not reach back to what was held: what lies between is unknown).
    public func replaceFabricTurns(_ key: FleetSessionKey, with turns: [FabricTurn], older: Bool, syncedThrough: Int? = nil) async throws {
        try await db.write { db in
            try db.execute(sql: "DELETE FROM fabric_turns WHERE runtime_id = ? AND session_id = ?", arguments: [key.runtimeID, key.id])
            try db.execute(sql: "DELETE FROM fabric_turn_sessions WHERE runtime_id = ? AND session_id = ?", arguments: [key.runtimeID, key.id])
        }
        try await cacheFabricTurns(turns.filter { $0.session == key }, olderBeyond: older, syncedThrough: syncedThrough)
    }

    /// Forget these sessions' turns (exited, or gone from the fleet).
    public func dropFabricTurns(_ keys: [FleetSessionKey]) async throws {
        guard !keys.isEmpty else { return }
        try await db.write { db in
            for k in keys { try Self.drop(db, k) }
        }
    }

    /// Forget every session's turns but these.
    public func dropFabricTurns(keeping keep: Set<FleetSessionKey>) async throws {
        try await db.write { db in
            let held = try Row.fetchAll(db, sql: """
                SELECT runtime_id, session_id FROM fabric_turns UNION SELECT runtime_id, session_id FROM fabric_turn_sessions
                """).map { FleetSessionKey(runtimeID: $0["runtime_id"], id: $0["session_id"]) }
            for k in held where !keep.contains(k) { try Self.drop(db, k) }
        }
    }

    /// These sessions turned Text off: keep their turns, not their words.
    public func blankFabricTurns(_ keys: [FleetSessionKey]) async throws {
        guard !keys.isEmpty else { return }
        let encoder = JSONEncoder()
        try await db.write { db in
            for k in keys {
                let rows = try Row.fetchAll(db, sql: "SELECT turn_id, body FROM fabric_turns WHERE runtime_id = ? AND session_id = ?",
                                            arguments: [k.runtimeID, k.id])
                for r in rows {
                    guard var t = try? JSONDecoder().decode(FabricTurn.self, from: r["body"] as Data),
                          !t.prompt.isEmpty || !t.reply.isEmpty else { continue }
                    t.textOff = true
                    let blank = Self.storable(t)
                    try db.execute(sql: "UPDATE fabric_turns SET body = ?, reply_length = 0 WHERE runtime_id = ? AND session_id = ? AND turn_id = ?",
                                   arguments: [try encoder.encode(blank), k.runtimeID, k.id, r["turn_id"] as String])
                }
            }
        }
    }

    /// How many turns are held, every session (tests, diagnostics).
    public func fabricTurnCount() async throws -> Int {
        try await db.read { db in try Int.fetchOne(db, sql: "SELECT count(*) FROM fabric_turns") ?? 0 }
    }

    // MARK: helpers

    /// What may be written: a Text off turn without its words.
    static func storable(_ t: FabricTurn) -> FabricTurn {
        guard t.textOff else { return t }
        var blank = t
        blank.prompt = ""
        blank.reply = ""
        return blank
    }

    private static func setOlder(_ db: Database, _ k: FleetSessionKey, _ older: Bool) throws {
        try db.execute(sql: """
            INSERT INTO fabric_turn_sessions(runtime_id, session_id, older) VALUES (?, ?, ?)
            ON CONFLICT(runtime_id, session_id) DO UPDATE SET older = excluded.older
            """, arguments: [k.runtimeID, k.id, older])
    }

    /// Move the watermark forward (never back).
    private static func setSynced(_ db: Database, _ k: FleetSessionKey, _ through: Int) throws {
        try db.execute(sql: """
            INSERT INTO fabric_turn_sessions(runtime_id, session_id, older, synced) VALUES (?, ?, 1, ?)
            ON CONFLICT(runtime_id, session_id) DO UPDATE SET synced = MAX(COALESCE(synced, excluded.synced), excluded.synced)
            """, arguments: [k.runtimeID, k.id, through])
    }

    private static func trim(_ db: Database, _ k: FleetSessionKey, keep: Int) throws {
        try db.execute(sql: """
            DELETE FROM fabric_turns WHERE runtime_id = ?1 AND session_id = ?2 AND turn_id NOT IN (
              SELECT turn_id FROM fabric_turns WHERE runtime_id = ?1 AND session_id = ?2 ORDER BY ended DESC, started DESC LIMIT ?3)
            """, arguments: [k.runtimeID, k.id, keep])
        // what was let go is still on the server: "Earlier turns" reads it
        if db.changesCount > 0 { try setOlder(db, k, true) }
    }

    private static func drop(_ db: Database, _ k: FleetSessionKey) throws {
        try db.execute(sql: "DELETE FROM fabric_turns WHERE runtime_id = ? AND session_id = ?", arguments: [k.runtimeID, k.id])
        try db.execute(sql: "DELETE FROM fabric_turn_sessions WHERE runtime_id = ? AND session_id = ?", arguments: [k.runtimeID, k.id])
    }
}
