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
    func markShareCommentsRead(_ shareID: String) async throws
    func replyToShareComment(_ shareID: String, body: String, parentID: String?, anchor: ShareAnchor?) async throws -> ShareComment
    func deleteShareComment(_ shareID: String, commentID: String) async throws
}

extension APIClient: ShareService {
    public func createShare(docID: DocID, snapshot: ShareSnapshot, expiresAt: Date?, commentsEnabled: Bool) async throws -> Share {
        try await shareSend("POST", "/api/shares", body: CreateShareBody(docID: docID, snapshot: snapshot, expiresAt: expiresAt, commentsEnabled: commentsEnabled))
    }

    /// The caller's own links, for one doc or (nil) every doc.
    public func shares(docID: DocID? = nil) async throws -> [Share] {
        struct List: Decodable { var shares: [Share] }
        let list: List = try await shareDecode(try await request("/api/shares", query: docID.map { [URLQueryItem(name: "doc_id", value: $0)] } ?? []))
        return list.shares
    }

    /// A snapshot republishes the same URL (`revision` goes up).
    public func updateShare(_ id: String, snapshot: ShareSnapshot? = nil, expiresAt: ShareExpiryChange = .keep, commentsEnabled: Bool? = nil) async throws -> Share {
        try await shareSend("PATCH", "/api/shares/\(Self.segment(id))", body: PatchShareBody(snapshot: snapshot, expiresAt: expiresAt, commentsEnabled: commentsEnabled))
    }

    /// Revoke: the token is dead for good (204).
    public func revokeShare(_ id: String) async throws {
        _ = try await shareData(try await request("/api/shares/\(Self.segment(id))", method: "DELETE"))
    }

    /// The exact page HTML for a snapshot, assets inlined (nothing stored): PDF export.
    public func sharePreviewHTML(_ snapshot: ShareSnapshot) async throws -> String {
        struct Body: Encodable { var snapshot: ShareSnapshot }
        struct Answer: Decodable { var html: String }
        let a: Answer = try await shareSend("POST", "/api/shares/preview", body: Body(snapshot: snapshot))
        return a.html
    }

    /// Reading doesn't mark them read: `markShareCommentsRead` does.
    public func shareComments(_ shareID: String) async throws -> [ShareComment] {
        struct List: Decodable { var comments: [ShareComment] }
        let list: List = try await shareDecode(try await request("/api/shares/\(Self.segment(shareID))/comments"))
        return list.comments
    }

    /// `POST /api/shares/{id}/comments/read`: the owner has seen them (any 2xx).
    public func markShareCommentsRead(_ shareID: String) async throws {
        var r = try await request("/api/shares/\(Self.segment(shareID))/comments/read", method: "POST")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = Data("{}".utf8)
        _ = try await shareData(r)
    }

    /// An owner reply (author = your display name, `is_owner`).
    public func replyToShareComment(_ shareID: String, body: String, parentID: String? = nil, anchor: ShareAnchor? = nil) async throws -> ShareComment {
        struct Body: Encodable {
            var body: String
            var parent_id: String?
            var anchor: ShareAnchor?
        }
        return try await shareSend("POST", "/api/shares/\(Self.segment(shareID))/comments", body: Body(body: body, parent_id: parentID, anchor: anchor))
    }

    public func deleteShareComment(_ shareID: String, commentID: String) async throws {
        _ = try await shareData(try await request("/api/shares/\(Self.segment(shareID))/comments/\(Self.segment(commentID))", method: "DELETE"))
    }

    /// One share round trip: a refusal with the server's `{"error"}` (403
    /// not yours to share, 413 too big, 429 a per-user cap, 400…) becomes a
    /// `ShareAPIError` carrying that message, retryable or not; everything
    /// else is checked as usual (401, HTML, 5xx without a message).
    func shareData(_ request: URLRequest) async throws -> Data {
        let (data, response) = try await data(for: request)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode), http.statusCode != 401,
           response.mimeType != "text/html", let message = Self.errorMessage(in: data) {
            throw ShareAPIError(status: http.statusCode, message: message)
        }
        if let http = response as? HTTPURLResponse, [403, 413, 429].contains(http.statusCode) {
            throw ShareAPIError(status: http.statusCode, message: nil)
        }
        try Self.check(data: data, response: response)
        return data
    }

    func shareDecode<T: Decodable>(_ request: URLRequest) async throws -> T {
        let data = try await shareData(request)
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw APIError.decoding(String(describing: error))
        }
    }

    func shareSend<T: Decodable>(_ method: String, _ path: String, body: some Encodable) async throws -> T {
        var r = try await request(path, method: method)
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(body)
        return try await shareDecode(r)
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

/// The server refused a share call, with its own words when it gave any.
public struct ShareAPIError: Error, Sendable, Hashable, LocalizedError {
    public var status: Int
    public var message: String?

    public init(status: Int, message: String?) {
        self.status = status
        self.message = message
    }

    public var errorDescription: String? {
        if let message, !message.isEmpty { return message }
        switch status {
        case 403: return "Only the doc's owner or an editor can share it."
        case 404: return "That link or doc isn't on this server."
        case 413: return "This doc is too large to share."
        case 429: return "You've reached the limit for now. Try again later."
        default: return "The server refused this (HTTP \(status))."
        }
    }

    /// A per-user cap or rate limit: worth trying later, not a mistake.
    public var isLimit: Bool { status == 429 }
}
