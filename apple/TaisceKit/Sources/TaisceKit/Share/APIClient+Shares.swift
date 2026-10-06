import Foundation

/// A field a PATCH may leave alone, set, or set to null.
public enum ShareExpiryChange: Sendable, Hashable {
    case keep
    case set(Date?)
}

/// The `/api/shares` routes (SERVER mode only; LOCAL answers 404). Human
/// only: the app's OAuth bearer works, a PAT or agent gets 403.
public protocol ShareService: Sendable {
    func createShare(docID: DocID, snapshot: ShareSnapshot, expiresAt: Date?, commentsEnabled: Bool) async throws -> Share
    func shares(docID: DocID?) async throws -> [Share]
    func updateShare(_ id: String, snapshot: ShareSnapshot?, expiresAt: ShareExpiryChange, commentsEnabled: Bool?) async throws -> Share
    func revokeShare(_ id: String) async throws
    func sharePreviewHTML(_ snapshot: ShareSnapshot) async throws -> String
    func shareComments(_ shareID: String) async throws -> [ShareComment]
    func replyToShareComment(_ shareID: String, body: String, parentID: String?, anchor: ShareAnchor?) async throws -> ShareComment
    func deleteShareComment(_ shareID: String, commentID: String) async throws
}

extension APIClient: ShareService {
    public func createShare(docID: DocID, snapshot: ShareSnapshot, expiresAt: Date?, commentsEnabled: Bool) async throws -> Share {
        try await post("/api/shares", body: CreateShareBody(docID: docID, snapshot: snapshot, expiresAt: expiresAt, commentsEnabled: commentsEnabled))
    }

    /// The caller's own links, for one doc or (nil) every doc.
    public func shares(docID: DocID? = nil) async throws -> [Share] {
        struct List: Decodable { var shares: [Share] }
        let list: List = try await get("/api/shares", query: docID.map { [URLQueryItem(name: "doc_id", value: $0)] } ?? [])
        return list.shares
    }

    /// A snapshot republishes the same URL (`revision` goes up).
    public func updateShare(_ id: String, snapshot: ShareSnapshot? = nil, expiresAt: ShareExpiryChange = .keep, commentsEnabled: Bool? = nil) async throws -> Share {
        try await send(body: PatchShareBody(snapshot: snapshot, expiresAt: expiresAt, commentsEnabled: commentsEnabled), method: "PATCH", path: "/api/shares/\(Self.segment(id))")
    }

    /// Revoke: the token is dead for good (204).
    public func revokeShare(_ id: String) async throws {
        try await sendNoContent(try await request("/api/shares/\(Self.segment(id))", method: "DELETE"))
    }

    /// The exact page HTML for a snapshot, assets inlined (nothing stored): PDF export.
    public func sharePreviewHTML(_ snapshot: ShareSnapshot) async throws -> String {
        struct Body: Encodable { var snapshot: ShareSnapshot }
        struct Answer: Decodable { var html: String }
        let a: Answer = try await post("/api/shares/preview", body: Body(snapshot: snapshot))
        return a.html
    }

    /// Reading them marks them read.
    public func shareComments(_ shareID: String) async throws -> [ShareComment] {
        struct List: Decodable { var comments: [ShareComment] }
        let list: List = try await get("/api/shares/\(Self.segment(shareID))/comments")
        return list.comments
    }

    /// An owner reply (author = your display name, `is_owner`).
    public func replyToShareComment(_ shareID: String, body: String, parentID: String? = nil, anchor: ShareAnchor? = nil) async throws -> ShareComment {
        struct Body: Encodable {
            var body: String
            var parent_id: String?
            var anchor: ShareAnchor?
        }
        return try await post("/api/shares/\(Self.segment(shareID))/comments", body: Body(body: body, parent_id: parentID, anchor: anchor))
    }

    public func deleteShareComment(_ shareID: String, commentID: String) async throws {
        try await sendNoContent(try await request("/api/shares/\(Self.segment(shareID))/comments/\(Self.segment(commentID))", method: "DELETE"))
    }

    /// A 204 (or any 2xx: its body is ignored).
    func sendNoContent(_ request: URLRequest) async throws {
        _ = try await send(raw: request)
    }

    /// An id as one path segment (ids are uuids; anything else is escaped).
    static func segment(_ s: String) -> String {
        s.addingPercentEncoding(withAllowedCharacters: .alphanumerics.union(CharacterSet(charactersIn: "-._~"))) ?? s
    }

    struct CreateShareBody: Encodable {
        var docID: DocID
        var snapshot: ShareSnapshot
        var expiresAt: Date?
        var commentsEnabled: Bool

        enum CodingKeys: String, CodingKey {
            case snapshot
            case docID = "doc_id"
            case expiresAt = "expires_at"
            case commentsEnabled = "comments_enabled"
        }

        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(docID, forKey: .docID)
            try c.encode(snapshot, forKey: .snapshot)
            // a required key: null = never expires
            try c.encode(expiresAt.map(ShareDate.string), forKey: .expiresAt)
            try c.encode(commentsEnabled, forKey: .commentsEnabled)
        }
    }

    struct PatchShareBody: Encodable {
        var snapshot: ShareSnapshot?
        var expiresAt: ShareExpiryChange
        var commentsEnabled: Bool?

        enum CodingKeys: String, CodingKey {
            case snapshot
            case expiresAt = "expires_at"
            case commentsEnabled = "comments_enabled"
        }

        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encodeIfPresent(snapshot, forKey: .snapshot)
            if case let .set(date) = expiresAt {
                try c.encode(date.map(ShareDate.string), forKey: .expiresAt)
            }
            try c.encodeIfPresent(commentsEnabled, forKey: .commentsEnabled)
        }
    }
}
