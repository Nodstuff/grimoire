import Foundation

/// One row of `GET /api/doc/{id}/history` (newest first): who wrote an op
/// and when. The op id is a UUIDv7, so its first 48 bits are the write time.
public struct DocHistoryEntry: Decodable, Sendable, Hashable {
    public var opID: String
    public var principalName: String
    public var principalKind: String
    public var applied: Bool
    /// the op's principal (`op.principal`); `Profile.principalID` is yours
    public var principalID: String?
    /// the epoch the op landed in (`op.epoch_applied`)
    public var epoch: Int?
    /// the block it wrote (`kind.target`, or `kind.block_id` for an insert);
    /// nil for doc ops (rename, move, status)
    public var targetBlock: BlockID?
    /// `insert`, `replace`, `delete`, `move`, `rename_doc`, …
    public var opType: String?
    /// the op's provenance (`review:decline:<id>` for a decline's revert,
    /// `rename:Old → New` for a link rewrite, …)
    public var sourceRefs: [String]
    /// what an insert or replace wrote
    public var content: String?
    /// the server's answer to "is this the caller, or one of the caller's
    /// own agents?" (`principal_is_yours`); nil from an older server
    public var principalIsYours: Bool?

    public init(
        opID: String, principalName: String, principalKind: String, applied: Bool = true,
        principalID: String? = nil, epoch: Int? = nil, targetBlock: BlockID? = nil, opType: String? = nil,
        sourceRefs: [String] = [], content: String? = nil, principalIsYours: Bool? = nil
    ) {
        self.opID = opID
        self.principalName = principalName
        self.principalKind = principalKind
        self.applied = applied
        self.principalID = principalID
        self.epoch = epoch
        self.targetBlock = targetBlock
        self.opType = opType
        self.sourceRefs = sourceRefs
        self.content = content
        self.principalIsYours = principalIsYours
    }

    /// When the op was written, from its UUIDv7 id; nil for any other id.
    public var date: Date? { Self.uuidV7Date(opID) }

    public static func uuidV7Date(_ id: String) -> Date? {
        let hex = id.replacingOccurrences(of: "-", with: "")
        guard hex.count == 32, hex[hex.index(hex.startIndex, offsetBy: 12)] == "7",
              let ms = UInt64(hex.prefix(12), radix: 16)
        else { return nil }
        return Date(timeIntervalSince1970: Double(ms) / 1000)
    }

    struct Op: Decodable {
        var id: String
        var epoch_applied: Int?
        var principal: String?
        var kind: Kind?
        var source_refs: [String]?

        struct Kind: Decodable {
            var op: String?
            var target: String?
            var block_id: String?
            var content: String?
        }
    }

    enum CodingKeys: String, CodingKey {
        case op
        case principalName = "principal_name"
        case principalKind = "principal_kind"
        case principalIsYours = "principal_is_yours"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let op = try c.decode(Op.self, forKey: .op)
        opID = op.id
        applied = op.epoch_applied != nil
        epoch = op.epoch_applied
        principalID = op.principal?.lowercased()
        opType = op.kind?.op
        targetBlock = (op.kind?.target ?? op.kind?.block_id)?.lowercased()
        sourceRefs = op.source_refs ?? []
        content = op.kind?.content
        principalName = try c.decodeIfPresent(String.self, forKey: .principalName) ?? ""
        principalKind = try c.decodeIfPresent(String.self, forKey: .principalKind) ?? ""
        principalIsYours = try c.decodeIfPresent(Bool.self, forKey: .principalIsYours)
    }
}

/// `GET /api/todo/parse?text=`: what the daemon would make of a typed
/// to-do (the live hint under the new-to-do field). Read-only.
public struct TodoParseHint: Decodable, Sendable, Hashable {
    public var text: String
    public var deadline: String?
    public var dueTime: String?
    /// a UTC-aware server answers the instant; older ones a local wall time
    public var dueAt: String?
    public var alertAt: String?
    public var warning: String?

    public init(text: String, deadline: String? = nil, dueTime: String? = nil, dueAt: String? = nil, alertAt: String? = nil, warning: String? = nil) {
        self.text = text
        self.deadline = deadline
        self.dueTime = dueTime
        self.dueAt = dueAt
        self.alertAt = alertAt
        self.warning = warning
    }

    /// `due_at` as an instant, else the day + wall time read on this
    /// device (the parse was asked with this device's `today`/`utc_offset`).
    public func deadlineValue(in timeZone: TimeZone = .current) -> Deadline? {
        if let dueAt, let d = Deadline(stored: dueAt) { return d }
        guard let deadline, let day = Due(dueTime.map { "\(deadline) \($0)" } ?? deadline) else { return nil }
        return Deadline.local(day, in: timeZone)
    }

    /// The deadline as a wall time in `timeZone`, for display.
    public func due(in timeZone: TimeZone = .current) -> Due? { deadlineValue(in: timeZone)?.due(in: timeZone) }

    enum CodingKeys: String, CodingKey {
        case text, deadline, warning
        case dueTime = "due_time"
        case dueAt = "due_at"
        case alertAt = "alert_at"
    }
}

/// `GET /api/profile`: the signed-in person (SERVER mode: the token's user;
/// LOCAL: the one human). `principal_id` is what the cache is keyed to.
public struct Profile: Decodable, Sendable, Hashable {
    public var name: String
    public var principalID: String?

    public init(name: String, principalID: String?) {
        self.name = name
        self.principalID = principalID
    }

    enum CodingKeys: String, CodingKey {
        case name
        case principalID = "principal_id"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? ""
        principalID = try c.decodeIfPresent(String.self, forKey: .principalID)
    }
}

extension APIClient {
    /// Who the token belongs to. Read-only. Bounded (10 s): sync waits on
    /// it at launch, and a resume without an answer just keeps its cache.
    public func profile() async throws -> Profile {
        var r = try await request("/api/profile")
        r.timeoutInterval = 10
        return try await send(r)
    }

    /// The doc's op ledger, newest first (the server caps it at 100).
    public func docHistory(_ id: DocID) async throws -> [DocHistoryEntry] {
        try await get("/api/doc/\(id)/history")
    }

    /// The server's reading of a typed to-do, e.g. "call Ann due fri 3pm",
    /// relative to this device's day and offset.
    public func todoParse(_ text: String, clock: TodoClock = TodoClock()) async throws -> TodoParseHint {
        try await get("/api/todo/parse", query: [
            URLQueryItem(name: "text", value: text),
            URLQueryItem(name: "today", value: clock.today),
            URLQueryItem(name: "utc_offset", value: clock.utcOffset),
        ])
    }
}
