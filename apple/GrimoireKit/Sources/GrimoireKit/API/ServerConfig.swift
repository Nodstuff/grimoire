import Foundation

/// Supplies a bearer token per request. OAuth + passkeys plug in here later;
/// `nil` means "send no Authorization header" (the localhost default).
public protocol TokenProvider: Sendable {
    func token() async throws -> String?
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
    case decoding(String)
    case badURL(String)
}
