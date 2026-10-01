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

    public enum CodingKeys: String, CodingKey, ColumnExpression {
        case id, method, path, body, state, attempts
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
    @discardableResult
    public func enqueue(_ req: ProposeRequest, now: Date = .now) async throws -> OutboxEntry {
        var req = req
        let key = req.requestID ?? UUID().uuidString.lowercased()
        req.requestID = key
        return try await enqueue(method: "POST", path: "/api/propose", body: JSONEncoder().encode(req), key: key, now: now)
    }

    /// Queue a deadline change (`nil` clears it). Same body as
    /// `APIClient.todoSetDeadline`.
    @discardableResult
    public func enqueueDeadline(date: String, itemID: String, deadline: Due?, key: String = UUID().uuidString.lowercased(), now: Date = .now) async throws -> OutboxEntry {
        let body = try JSONEncoder().encode(APIClient.DeadlineBody(date: date, itemID: itemID, deadline: deadline))
        return try await enqueue(method: "POST", path: "/api/todo/deadline", body: body, key: key, now: now)
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

    public func markOutbox(_ id: Int64, state: OutboxEntry.State, error: String? = nil) async throws {
        try await db.write { db in
            try db.execute(
                sql: "UPDATE outbox SET state = ?, last_error = ?, attempts = attempts + (CASE WHEN ? = 'inflight' THEN 1 ELSE 0 END) WHERE id = ?",
                arguments: [state.rawValue, error, state.rawValue, id]
            )
        }
    }
}

/// Replays the outbox in order. STUB: sends each entry once and marks it
/// done/failed; there is no conflict handling yet (a stale `base_epoch`
/// comes back scored or red from the gate, which the UI will surface later).
public struct OutboxReplayer: Sendable {
    let api: APIClient
    let cache: Cache

    public init(api: APIClient, cache: Cache) {
        self.api = api
        self.cache = cache
    }

    public func replay() async throws {
        for entry in try await cache.pendingOutbox() {
            guard let id = entry.id else { continue }
            try await cache.markOutbox(id, state: .inflight)
            do {
                var r = try await api.request(entry.path, method: entry.method)
                r.setValue("application/json", forHTTPHeaderField: "Content-Type")
                r.httpBody = entry.body
                _ = try await api.send(raw: r)
                try await cache.markOutbox(id, state: .done)
            } catch APIError.unauthorized {
                // signed out or mid-refresh: not this entry's fault, keep it and the order
                try await cache.markOutbox(id, state: .pending, error: String(describing: APIError.unauthorized))
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
}
