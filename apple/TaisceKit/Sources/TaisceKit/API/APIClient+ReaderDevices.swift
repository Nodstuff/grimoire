import Foundation

/// A paired e-reader (`/api/ereaders`, SERVER mode, the person's own app):
/// it reads the OPDS catalog and syncs progress with its own password.
public struct DevicePasswordReader: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var label: String
    public var streamDownloads: Bool
    public var createdAt: String
    public var lastUsedAt: String?
    public var revokedAt: String?

    public init(id: String, label: String, streamDownloads: Bool = false, createdAt: String, lastUsedAt: String? = nil, revokedAt: String? = nil) {
        self.id = id
        self.label = label
        self.streamDownloads = streamDownloads
        self.createdAt = createdAt
        self.lastUsedAt = lastUsedAt
        self.revokedAt = revokedAt
    }

    enum CodingKeys: String, CodingKey {
        case id, label
        case streamDownloads = "stream_downloads"
        case createdAt = "created_at"
        case lastUsedAt = "last_used_at"
        case revokedAt = "revoked_at"
    }

    public var isActive: Bool { revokedAt == nil }
}

/// A token that may only add books (`/api/upload-tokens`): for a Shortcut
/// or Calibre. The secret exists only in the create answer.
public struct UploadTokenInfo: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var label: String
    public var createdAt: String
    public var lastUsedAt: String?
    public var revokedAt: String?

    public init(id: String, label: String, createdAt: String, lastUsedAt: String? = nil, revokedAt: String? = nil) {
        self.id = id
        self.label = label
        self.createdAt = createdAt
        self.lastUsedAt = lastUsedAt
        self.revokedAt = revokedAt
    }

    enum CodingKeys: String, CodingKey {
        case id, label
        case createdAt = "created_at"
        case lastUsedAt = "last_used_at"
        case revokedAt = "revoked_at"
    }

    public var isActive: Bool { revokedAt == nil }
}

/// What a device needs to be set up, shown once after pairing (or a new password).
public struct DevicePasswordSetup: Sendable, Hashable {
    /// the person's Taisce name: the OPDS and progress-sync username
    public var username: String?
    public var password: String
    /// OPDS catalog, with the username and password
    public var opds: String
    /// OPDS catalog for readers without a login field: the password in the path
    public var opdsPath: String
    /// KOReader's progress sync server
    public var progressSync: String

    public init(server: String, username: String?, password: String) {
        var base = server
        while base.hasSuffix("/") { base.removeLast() }
        let encoded = password.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed.subtracting(CharacterSet(charactersIn: "/"))) ?? password
        self.username = username
        self.password = password
        opds = "\(base)/opds/"
        opdsPath = "\(base)/opds/\(encoded)/"
        progressSync = "\(base)/kosync"
    }
}

/// The server's longest device or token name (opds.rs MAX_LABEL: 60).
public let readerDeviceLabelMax = 60

extension APIClient {
    public func ereaders() async throws -> [DevicePasswordReader] {
        struct Body: Decodable { var ereaders: [DevicePasswordReader] }
        let b: Body = try await get("/api/ereaders")
        return b.ereaders
    }

    /// Pair a device: its row and its password (shown once, never stored here).
    public func pairEreader(label: String, streamDownloads: Bool = false) async throws -> (DevicePasswordReader, String) {
        struct Req: Encodable { var label: String; var stream_downloads: Bool }
        struct Body: Decodable { var ereader: DevicePasswordReader; var password: String }
        let b: Body = try await post("/api/ereaders", body: Req(label: label, stream_downloads: streamDownloads))
        return (b.ereader, b.password)
    }

    /// A new password for the device (the old one stops working).
    public func rotateEreader(_ id: String) async throws -> String {
        struct Empty: Encodable {}
        struct Body: Decodable { var password: String }
        let b: Body = try await post("/api/ereaders/\(id)/rotate", body: Empty())
        return b.password
    }

    public func removeEreader(_ id: String) async throws {
        _ = try await send(raw: try await request("/api/ereaders/\(id)", method: "DELETE"))
    }

    public func uploadTokens() async throws -> [UploadTokenInfo] {
        struct Body: Decodable { var upload_tokens: [UploadTokenInfo] }
        let b: Body = try await get("/api/upload-tokens")
        return b.upload_tokens
    }

    /// A new token: its row and its secret (shown once, never stored here).
    public func createUploadToken(label: String) async throws -> (UploadTokenInfo, String) {
        struct Req: Encodable { var label: String }
        struct Body: Decodable { var upload_token: UploadTokenInfo; var token: String }
        let b: Body = try await post("/api/upload-tokens", body: Req(label: label))
        return (b.upload_token, b.token)
    }

    public func revokeUploadToken(_ id: String) async throws {
        _ = try await send(raw: try await request("/api/upload-tokens/\(id)", method: "DELETE"))
    }
}
