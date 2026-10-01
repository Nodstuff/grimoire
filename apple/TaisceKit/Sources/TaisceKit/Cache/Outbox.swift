import Foundation
import GRDB

/// A write queued while offline (or in flight). The idempotency key doubles
/// as the server's `request_id`, so a replay after a lost response returns
/// the first outcome instead of applying twice.
public struct OutboxEntry: Codable, Sendable, Hashable, FetchableRecord, MutablePersistableRecord {
    public static let databaseTableName = "outbox"

    public enum State: String, Codable, Sendable {
        case pending, inflight, done, failed
    }

    public var id: Int64?
    public var createdAt: Date
    public var idempotencyKey: String
    public var method: String
    public var path: String
    public var body: Data?
    public var state: State
    public var attempts: Int
    public var lastError: String?
    /// the server's answer to a landed propose
    public var outcome: Data?

    public enum CodingKeys: String, CodingKey, ColumnExpression {
        case id, method, path, body, state, attempts, outcome
        case createdAt = "created_at"
        case idempotencyKey = "idempotency_key"
        case lastError = "last_error"
    }

    public typealias Columns = CodingKeys

    public mutating func didInsert(_ inserted: InsertionSuccess) {
        id = inserted.rowID
    }
}

extension Cache {
    /// Queue a propose. Re-enqueueing the same `requestID` is a no-op.
    /// A base epoch our own landed writes have since moved past (each one
    /// `base → base + 1`, nobody else in between) is moved up with them,
    /// so an edit made on top of them isn't scored as stale against them.
    @discardableResult
    public func enqueue(_ request: ProposeRequest, now: Date = .now) async throws -> OutboxEntry {
        let key = request.requestID ?? UUID().uuidString.lowercased()
        return try await db.write { db in
            if let existing = try OutboxEntry.filter(OutboxEntry.Columns.idempotencyKey == key).fetchOne(db) {
                return existing
            }
            var req = request
            req.requestID = key
            req.baseEpoch = try Self.chainedBase(db, doc: req.docID, from: req.baseEpoch)
            var e = OutboxEntry(
                id: nil, createdAt: now, idempotencyKey: key, method: "POST", path: "/api/propose",
                body: try JSONEncoder().encode(req), state: .pending, attempts: 0, lastError: nil
            )
            try e.insert(db)
            return e
        }
    }

    /// Queue a text save: when the newest queued write is a never-sent
    /// replace of the same block, rewrite it instead of adding another
    /// (typing in one block makes one write, not one per pause). Anything
    /// else (another block, a structure op, a row already tried) queues anew.
    @discardableResult
    public func enqueueCoalescing(_ req: ProposeRequest, now: Date = .now) async throws -> OutboxEntry {
        guard req.ops.count == 1, case let .replace(target, _) = req.ops[0].kind else {
            return try await enqueue(req, now: now)
        }
        let merged: OutboxEntry? = try await db.write { db in
            guard var last = try OutboxEntry
                .filter([OutboxEntry.State.pending.rawValue, OutboxEntry.State.inflight.rawValue].contains(OutboxEntry.Columns.state))
                .order(OutboxEntry.Columns.id.desc).fetchOne(db),
                last.state == .pending, last.attempts == 0, last.path == "/api/propose", let body = last.body,
                var prev = try? JSONDecoder().decode(ProposeRequest.self, from: body), prev.docID == req.docID,
                prev.ops.count == 1, case .replace(target, _) = prev.ops[0].kind
            else { return nil }
            prev.ops = req.ops
            last.body = try JSONEncoder().encode(prev)
            try last.update(db)
            return last
        }
        if let merged { return merged }
        return try await enqueue(req, now: now)
    }

    /// Follow our own landed proposes up from `base`.
    static func chainedBase(_ db: Database, doc: DocID, from base: Int) throws -> Int {
        var base = base
        let landed = try OutboxEntry
            .filter(OutboxEntry.Columns.state == OutboxEntry.State.done.rawValue)
            .filter(OutboxEntry.Columns.createdAt >= Date().addingTimeInterval(-Cache.reviewWindow))
            .filter(OutboxEntry.Columns.path == "/api/propose" || OutboxEntry.Columns.path == "/api/propose_markdown")
            .filter(OutboxEntry.Columns.outcome != nil)
            .fetchAll(db)
        var steps: [Int: Int] = [:]
        for e in landed {
            guard let (d, b) = OutboxReplayer.proposeBase(e), d == doc, let data = e.outcome,
                  let out = try? JSONDecoder().decode(ProposeOutcome.self, from: data), out.epoch == b + 1
            else { continue }
            steps[b] = out.epoch
        }
        var seen: Set<Int> = []
        while let next = steps[base], seen.insert(base).inserted { base = next }
        return base
    }

    /// Queue a deadline change (`nil` clears it). Same body as
    /// `APIClient.todoSetDeadline`; the clock is the one at queue time.
    @discardableResult
    public func enqueueDeadline(date: String, itemID: String, deadline: Deadline?, clock: TodoClock = TodoClock(), key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        let body = try JSONEncoder().encode(APIClient.DeadlineBody(date: date, itemID: itemID, deadline: deadline, clock: clock))
        return try await enqueue(method: "POST", path: "/api/todo/deadline", body: body, key: key, now: now)
    }

    /// A wall-clock `Due` picked on this device (date only = all-day).
    @discardableResult
    public func enqueueDeadline(date: String, itemID: String, deadline: Due?, clock: TodoClock = TodoClock(), key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        try await enqueueDeadline(date: date, itemID: itemID, deadline: deadline.flatMap { Deadline.local($0) }, clock: clock, key: key, now: now)
    }

    /// Queue any to-do write with the device's clock in the body (SERVER
    /// mode refuses one without `today`): toggle, add, edit, remove, move, note.
    @discardableResult
    public func enqueueTodo(_ path: String, date: String, itemID: String? = nil, text: String? = nil, done: Bool? = nil, toDate: String? = nil, note: String? = nil, clock: TodoClock = TodoClock(), key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        let body = try JSONEncoder().encode(APIClient.TodoBody(date: date, itemID: itemID, text: text, done: done, toDate: toDate, note: note, clock: clock))
        return try await enqueue(method: "POST", path: path, body: body, key: key, now: now)
    }

    @discardableResult
    public func enqueue(method: String, path: String, body: Data?, key: String, now: Date = .now) async throws -> OutboxEntry {
        try await db.write { db in
            if let existing = try OutboxEntry.filter(OutboxEntry.Columns.idempotencyKey == key).fetchOne(db) {
                return existing
            }
            var e = OutboxEntry(
                id: nil, createdAt: now, idempotencyKey: key, method: method, path: path,
                body: body, state: .pending, attempts: 0, lastError: nil
            )
            try e.insert(db)
            return e
        }
    }

    /// Pending entries, oldest first: the replay order.
    public func pendingOutbox() async throws -> [OutboxEntry] {
        try await db.read { db in
            try OutboxEntry
                .filter([OutboxEntry.State.pending.rawValue, OutboxEntry.State.inflight.rawValue].contains(OutboxEntry.Columns.state))
                .order(OutboxEntry.Columns.id)
                .fetchAll(db)
        }
    }

    public func outboxEntry(_ id: Int64) async throws -> OutboxEntry? {
        try await db.read { db in try OutboxEntry.fetchOne(db, key: id) }
    }

    public func markOutbox(_ id: Int64, state: OutboxEntry.State, error: String? = nil, outcome: Data? = nil) async throws {
        try await db.write { db in
            try db.execute(
                sql: "UPDATE outbox SET state = ?, last_error = ?, outcome = COALESCE(?, outcome), attempts = attempts + (CASE WHEN ? = 'inflight' THEN 1 ELSE 0 END) WHERE id = ?",
                arguments: [state.rawValue, error, outcome, state.rawValue, id]
            )
        }
    }

    /// Take a pending entry for sending: marks it inflight and returns it
    /// as of that moment, in one write (a coalescing save can't slip a new
    /// body in between reading and sending).
    public func claimOutbox(_ id: Int64) async throws -> OutboxEntry? {
        try await db.write { db in
            guard var e = try OutboxEntry.fetchOne(db, key: id), e.state == .pending || e.state == .inflight else { return nil }
            e.state = .inflight
            e.attempts += 1
            try e.update(db)
            return e
        }
    }

    /// Entries the server refused, oldest first.
    public func failedOutbox() async throws -> [OutboxEntry] {
        try await db.read { db in
            try OutboxEntry.filter(OutboxEntry.Columns.state == OutboxEntry.State.failed.rawValue).order(OutboxEntry.Columns.id).fetchAll(db)
        }
    }

    /// Put refused entries for `docID` back in the queue (after the cause
    /// was dealt with, e.g. a doc that was read-only).
    public func retryFailed(docID: DocID) async throws {
        let ids = try await failedOutbox().filter { OutboxReplayer.proposeBase($0)?.0 == docID }.compactMap(\.id)
        try await db.write { db in
            for id in ids {
                try db.execute(sql: "UPDATE outbox SET state = 'pending', last_error = NULL WHERE id = ?", arguments: [id])
            }
        }
    }

    /// Drop refused entries for `docID` (the user gave up on them).
    public func discardFailed(docID: DocID) async throws {
        let ids = try await failedOutbox().filter { OutboxReplayer.proposeBase($0)?.0 == docID }.compactMap(\.id)
        try await db.write { db in
            for id in ids { _ = try OutboxEntry.deleteOne(db, key: id) }
        }
    }
}

extension Cache {
    /// A propose for `docID` landed at `epoch`: queued proposes for the same
    /// doc still based on `base` were written on top of it, so move them to
    /// `epoch` (they would otherwise score as stale against our own write).
    public func rebaseOutbox(docID: DocID, from base: Int, to epoch: Int) async throws {
        guard base != epoch else { return }
        try await db.write { db in
            let pending = try OutboxEntry
                .filter(OutboxEntry.Columns.state == OutboxEntry.State.pending.rawValue)
                .filter(OutboxEntry.Columns.path == "/api/propose" || OutboxEntry.Columns.path == "/api/propose_markdown")
                .fetchAll(db)
            for var e in pending {
                guard let body = e.body,
                      var obj = try JSONSerialization.jsonObject(with: body) as? [String: Any],
                      obj["doc_id"] as? String == docID, obj["base_epoch"] as? Int == base
                else { continue }
                obj["base_epoch"] = epoch
                e.body = try JSONSerialization.data(withJSONObject: obj, options: [.sortedKeys])
                try e.update(db)
            }
        }
    }
}

/// Replays the outbox in order. Each entry is sent once and marked
/// done/failed; a propose that lands rebases the queued proposes chained on
/// it. No conflict UI yet: a stale `base_epoch` (someone else wrote first)
/// comes back scored or red from the gate, and the UI will surface it later.
public struct OutboxReplayer: Sendable {
    let api: APIClient
    let cache: Cache

    public init(api: APIClient, cache: Cache) {
        self.api = api
        self.cache = cache
    }

    public func replay() async throws {
        for queued in try await cache.pendingOutbox() {
            // claim as it stands now: an earlier landing may have rebased it,
            // a coalescing save may have rewritten it
            guard let id = queued.id, let entry = try await cache.claimOutbox(id) else { continue }
            do {
                var r = try await api.request(entry.path, method: entry.method)
                r.setValue("application/json", forHTTPHeaderField: "Content-Type")
                r.httpBody = entry.body
                let data = try await api.send(raw: r)
                let isPropose = entry.path == "/api/propose" || entry.path == "/api/propose_markdown"
                try await cache.markOutbox(id, state: .done, outcome: isPropose ? data : nil)
                // rebase only onto our own write: an epoch that jumped further
                // means someone else wrote too, and the gate should score the rest
                if let (doc, base) = Self.proposeBase(entry), let outcome = try? JSONDecoder().decode(Landed.self, from: data),
                   outcome.epoch == base + 1 {
                    try await cache.rebaseOutbox(docID: doc, from: base, to: outcome.epoch)
                }
            } catch let APIError.server(msg) where msg.hasPrefix(Self.liveSessionRefusal) {
                // a live session freezes the doc's epoch: retry after it ends
                try await cache.markOutbox(id, state: .pending, error: msg)
                return
            } catch APIError.unauthorized {
                // signed out or mid-refresh: not this entry's fault, keep it and the order
                try await cache.markOutbox(id, state: .pending, error: String(describing: APIError.unauthorized))
                return
            } catch let e as APIError where e.isTransient {
                // 5xx, 408, 429, or a proxy's HTML page: try again later, keep order
                try await cache.markOutbox(id, state: .pending, error: String(describing: e))
                return
            } catch let e as APIError {
                // the server answered: retrying the same request won't help
                try await cache.markOutbox(id, state: .failed, error: String(describing: e))
            } catch {
                // transport failure: leave it for the next replay, keep order
                try await cache.markOutbox(id, state: .pending, error: String(describing: error))
                return
            }
        }
    }

    /// `HotRegistry::assert_cold`'s refusal (crates/daemon/src/hot.rs).
    static let liveSessionRefusal = "doc is in a live session"

    private struct Landed: Decodable { var epoch: Int }
    private struct Based: Decodable {
        var doc_id: DocID
        var base_epoch: Int
    }

    static func proposeBase(_ e: OutboxEntry) -> (DocID, Int)? {
        guard e.path == "/api/propose" || e.path == "/api/propose_markdown", let body = e.body,
              let b = try? JSONDecoder().decode(Based.self, from: body)
        else { return nil }
        return (b.doc_id, b.base_epoch)
    }
}
