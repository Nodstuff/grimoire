import Foundation
import GRDB

/// What the gate said about a block this device edited, while it matters:
/// yellow = applied and flagged for a human, red = parked until one accepts.
public struct BlockReview: Hashable, Sendable {
    public var verdict: Verdict
    public var note: String
    public var opID: String
    /// the doc epoch the outcome reported
    public var epoch: Int
}

/// The outbox as one doc's editor sees it.
public struct DocOutboxState: Hashable, Sendable {
    /// queued or in flight
    public var pending = 0
    /// refused by the server (kept, never dropped silently)
    public var failed = 0
    /// the newest refusal's message
    public var failure: String?
    /// a live (hot) session refused the last try: it waits for the session to end
    public var liveSession = false

    public init(pending: Int = 0, failed: Int = 0, failure: String? = nil, liveSession: Bool = false) {
        self.pending = pending
        self.failed = failed
        self.failure = failure
        self.liveSession = liveSession
    }
}

extension Cache {
    /// How long landed proposes are remembered (review marks, base chaining):
    /// the server keeps `request_id` replays for 7 days too.
    public static let reviewWindow: TimeInterval = 7 * 24 * 3600

    /// Yellow and red verdicts on `docID`'s blocks from this device's landed
    /// proposes, newest per block (a later green clears it). With
    /// `openOps` (the doc's review queue, when online), only verdicts
    /// whose op still waits for a human count.
    public func reviews(for docID: DocID, openOps: Set<String>? = nil, now: Date = .now) async throws -> [BlockID: BlockReview] {
        let rows = try await db.read { db in
            try OutboxEntry
                .filter(OutboxEntry.Columns.state == OutboxEntry.State.done.rawValue)
                .filter(OutboxEntry.Columns.path == "/api/propose")
                .filter(OutboxEntry.Columns.outcome != nil)
                .filter(OutboxEntry.Columns.createdAt >= now.addingTimeInterval(-Cache.reviewWindow))
                .order(OutboxEntry.Columns.id)
                .fetchAll(db)
        }
        var out: [BlockID: BlockReview] = [:]
        for e in rows {
            guard let body = e.body, let data = e.outcome,
                  let req = try? JSONDecoder().decode(ProposeRequest.self, from: body), req.docID == docID,
                  let outcome = try? JSONDecoder().decode(ProposeOutcome.self, from: data)
            else { continue }
            for (i, v) in outcome.verdicts.enumerated() {
                let target = v.blockID ?? (i < req.ops.count ? req.ops[i].kind.target : nil)
                guard let target else { continue }
                if v.verdict == .green || (openOps.map { !$0.contains(v.opID) } ?? false) {
                    out[target] = nil
                } else {
                    out[target] = BlockReview(verdict: v.verdict, note: v.note, opID: v.opID, epoch: outcome.epoch)
                }
            }
        }
        return out
    }

    /// The newest epoch one of our proposes for `docID` landed at: a cached
    /// body older than this predates our own write.
    public func lastLandedEpoch(_ docID: DocID) async throws -> Int? {
        let rows = try await db.read { db in
            try OutboxEntry
                .filter(OutboxEntry.Columns.state == OutboxEntry.State.done.rawValue)
                .filter(OutboxEntry.Columns.path == "/api/propose" || OutboxEntry.Columns.path == "/api/propose_markdown")
                .filter(OutboxEntry.Columns.outcome != nil)
                .order(OutboxEntry.Columns.id.desc)
                .limit(20)
                .fetchAll(db)
        }
        return rows.compactMap { e -> Int? in
            guard OutboxReplayer.proposeBase(e)?.0 == docID, let data = e.outcome else { return nil }
            return (try? JSONDecoder().decode(ProposeOutcome.self, from: data))?.epoch
        }.max()
    }

    public func outboxState(for docID: DocID) async throws -> DocOutboxState {
        let (pending, failed) = try await (pendingOutbox(), failedOutbox())
        var s = DocOutboxState()
        for e in pending where OutboxReplayer.proposeBase(e)?.0 == docID {
            s.pending += 1
            if e.lastError?.contains(OutboxReplayer.liveSessionRefusal) == true { s.liveSession = true }
        }
        for e in failed where OutboxReplayer.proposeBase(e)?.0 == docID {
            s.failed += 1
            s.failure = e.lastError
        }
        return s
    }
}

extension BlockOp {
    /// The block an op writes.
    public var target: BlockID? {
        switch self {
        case let .insert(id, _, _, _, _): id
        case let .replace(t, _), let .delete(t), let .move(t, _, _): t
        }
    }
}
