import Foundation

/// Supplies a bearer token per request (`AuthSession` for a server-mode
/// daemon); `nil` means "send no Authorization header" (the localhost default).
public protocol TokenProvider: Sendable {
    func token() async throws -> String?
    /// The server answered 401 to `rejected`: a fresh token to retry with
    /// once, or nil to give up. Called concurrently by every request that
    /// got the 401, so it must refresh at most once per rejected token.
    func renew(rejected: String) async throws -> String?
}

extension TokenProvider {
    public func renew(rejected: String) async throws -> String? { nil }
}

public struct NoAuth: TokenProvider {
    public init() {}
    public func token() async throws -> String? { nil }
}

public struct ServerConfig: Sendable {
    public static let localDefault = URL(string: "http://127.0.0.1:7425").map { ServerConfig(baseURL: $0) }

    public var baseURL: URL
    public var tokenProvider: any TokenProvider

    public init(baseURL: URL, tokenProvider: any TokenProvider = NoAuth()) {
        self.baseURL = baseURL
        self.tokenProvider = tokenProvider
    }
}

public enum APIError: Error, Sendable, Equatable {
    /// The daemon answers most failures as HTTP 200 `{"error": "..."}`.
    case server(String)
    case http(status: Int)
    /// 401 even after one token renewal: sign in again.
    case unauthorized
    case decoding(String)
    case badURL(String)
    /// HTML where JSON was expected: the daemon's SPA fallback answers
    /// unknown routes (e.g. an older daemon without /api/changes) with 200 + index.html
    case notAPIRoute(String)
}
