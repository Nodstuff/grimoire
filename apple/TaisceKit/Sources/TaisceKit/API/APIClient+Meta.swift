import Foundation

/// One row of `GET /api/doc/{id}/history` (newest first): who wrote an op
/// and when. The op id is a UUIDv7, so its first 48 bits are the write time.
public struct DocHistoryEntry: Decodable, Sendable, Hashable {
    public var opID: String
    public var principalName: String
    public var principalKind: String
    public var applied: Bool

    public init(opID: String, principalName: String, principalKind: String, applied: Bool = true) {
        self.opID = opID
        self.principalName = principalName
        self.principalKind = principalKind
        self.applied = applied
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
    }

    enum CodingKeys: String, CodingKey {
        case op
        case principalName = "principal_name"
        case principalKind = "principal_kind"
    }

    public init(from decoder: any Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let op = try c.decode(Op.self, forKey: .op)
        opID = op.id
        applied = op.epoch_applied != nil
        principalName = try c.decodeIfPresent(String.self, forKey: .principalName) ?? ""
        principalKind = try c.decodeIfPresent(String.self, forKey: .principalKind) ?? ""
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
    /// Who the token belongs to. Read-only.
    public func profile() async throws -> Profile {
        try await get("/api/profile")
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
