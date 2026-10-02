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
    /// What a person typed into "Server URL", as a base URL: trimmed, no
    /// trailing slash, and a scheme when none was typed (https, except http
    /// for a loopback daemon, which only speaks plain HTTP). Nil without a host.
    public static func normalizedURL(_ input: String) -> URL? {
        var s = input.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !s.isEmpty else { return nil }
        if !s.contains("://") {
            let authority = s.split(separator: "/").first.map(String.init) ?? s
            let host = authority.hasPrefix("[")
                ? String(authority.dropFirst().prefix { $0 != "]" })
                : String(authority.split(separator: ":").first ?? "")
            s = (["127.0.0.1", "localhost", "::1"].contains(host) ? "http://" : "https://") + s
        }
        guard var c = URLComponents(string: s), let scheme = c.scheme?.lowercased(),
              scheme == "http" || scheme == "https", let host = c.host, !host.isEmpty
        else { return nil }
        c.scheme = scheme
        while c.path.hasSuffix("/") { c.path.removeLast() }
        return c.url
    }

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
    /// A 404, or the daemon's 200 `{"error": "not found: …"}` (older builds)
    case notFound(String)
}

extension APIError {
    /// Worth retrying later: the server (or something in front of it) is
    /// unwell or busy, not refusing this request.
    public var isTransient: Bool {
        switch self {
        case let .http(status): status >= 500 || status == 408 || status == 429
        case .notAPIRoute: true
        default: false
        }
    }
}
