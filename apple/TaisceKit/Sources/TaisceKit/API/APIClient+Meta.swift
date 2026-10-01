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
    public var alertAt: String?
    public var warning: String?

    public init(text: String, deadline: String? = nil, dueTime: String? = nil, alertAt: String? = nil, warning: String? = nil) {
        self.text = text
        self.deadline = deadline
        self.dueTime = dueTime
        self.alertAt = alertAt
        self.warning = warning
    }

    public var due: Due? {
        guard let deadline else { return nil }
        return Due(dueTime.map { "\(deadline) \($0)" } ?? deadline)
    }

    enum CodingKeys: String, CodingKey {
        case text, deadline, warning
        case dueTime = "due_time"
        case alertAt = "alert_at"
    }
}

extension APIClient {
    /// The doc's op ledger, newest first (the server caps it at 100).
    public func docHistory(_ id: DocID) async throws -> [DocHistoryEntry] {
        try await get("/api/doc/\(id)/history")
    }

    /// The server's reading of a typed to-do, e.g. "call Ann due fri 3pm".
    public func todoParse(_ text: String) async throws -> TodoParseHint {
        try await get("/api/todo/parse", query: [URLQueryItem(name: "text", value: text)])
    }
}
