import Foundation

/// RFC 9728 protected-resource metadata (`/.well-known/oauth-protected-resource`).
public struct ProtectedResourceMetadata: Codable, Sendable, Hashable {
    public var resource: String
    public var authorizationServers: [String]
    public var scopesSupported: [String]?
    public var resourceName: String?

    enum CodingKeys: String, CodingKey {
        case resource
        case authorizationServers = "authorization_servers"
        case scopesSupported = "scopes_supported"
        case resourceName = "resource_name"
    }
}

/// RFC 8414 authorization-server metadata (`/.well-known/oauth-authorization-server`).
public struct AuthorizationServerMetadata: Codable, Sendable, Hashable {
    public var issuer: String
    public var authorizationEndpoint: URL
    public var tokenEndpoint: URL
    public var registrationEndpoint: URL?
    public var revocationEndpoint: URL?
    public var scopesSupported: [String]?
    public var codeChallengeMethodsSupported: [String]?
    public var tokenEndpointAuthMethodsSupported: [String]?
    public var authorizationResponseIssParameterSupported: Bool?

    enum CodingKeys: String, CodingKey {
        case issuer
        case authorizationEndpoint = "authorization_endpoint"
        case tokenEndpoint = "token_endpoint"
        case registrationEndpoint = "registration_endpoint"
        case revocationEndpoint = "revocation_endpoint"
        case scopesSupported = "scopes_supported"
        case codeChallengeMethodsSupported = "code_challenge_methods_supported"
        case tokenEndpointAuthMethodsSupported = "token_endpoint_auth_methods_supported"
        case authorizationResponseIssParameterSupported = "authorization_response_iss_parameter_supported"
    }
}

/// Both discovery documents for one server.
public struct OAuthDiscovery: Sendable, Hashable {
    public var resource: ProtectedResourceMetadata
    public var server: AuthorizationServerMetadata
}

/// What the Keychain keeps per server after a sign-in or refresh.
public struct TokenSet: Codable, Sendable, Hashable {
    public var accessToken: String
    public var refreshToken: String
    public var expiresAt: Date
    public var scope: String?

    public init(accessToken: String, refreshToken: String, expiresAt: Date, scope: String? = nil) {
        self.accessToken = accessToken
        self.refreshToken = refreshToken
        self.expiresAt = expiresAt
        self.scope = scope
    }
}

/// The token endpoint's success body (RFC 6749 §5.1).
struct TokenResponse: Decodable {
    var accessToken: String
    var tokenType: String
    var expiresIn: Int?
    var refreshToken: String?
    var scope: String?

    enum CodingKeys: String, CodingKey {
        case scope
        case accessToken = "access_token"
        case tokenType = "token_type"
        case expiresIn = "expires_in"
        case refreshToken = "refresh_token"
    }
}

/// Where the sign-in browser hands back the code: the app's custom scheme
/// (any app could declare it), or a universal link on the server's own host
/// that only this app can claim (`apple-app-site-association`).
public enum OAuthCallback: Sendable, Hashable {
    case customScheme(String)
    case https(host: String, path: String)

    public static let custom = OAuthCallback.customScheme(OAuthClient.callbackScheme)

    /// The `redirect_uri` the server registers and redirects to.
    public var redirectURI: String {
        switch self {
        case .customScheme: OAuthClient.redirectURI
        case let .https(host, path): "https://\(host)\(path)"
        }
    }

    /// `ASWebAuthenticationSession`'s `.https(host:path:)` callback exists
    /// from iOS 17.4 and macOS 14.4 (Mac Catalyst reports the macOS
    /// version). The app's deployment targets (26) are above both today;
    /// this keeps the choice honest if they ever drop.
    public static func httpsSupported(_ v: OperatingSystemVersion, mac: Bool) -> Bool {
        let floor = mac ? (14, 4) : (17, 4)
        return (v.majorVersion, v.minorVersion) >= floor
    }

    public static var runningOnMac: Bool {
        #if targetEnvironment(macCatalyst) || os(macOS)
        true
        #else
        false
        #endif
    }

    /// The universal-link callback for `server`, when the OS supports it,
    /// the server is https and the app claims its host; else nil (use the
    /// custom scheme).
    public static func appLink(for server: URL, claimedHosts: Set<String>, available: Bool) -> OAuthCallback? {
        guard available, server.scheme?.lowercased() == "https", let host = server.host()?.lowercased(),
              claimedHosts.contains(where: { $0.lowercased() == host }), server.port == nil || server.port == 443
        else { return nil }
        return .https(host: host, path: OAuthClient.appLinkPath)
    }
}

/// An authorization request in flight: the URL to open and the secrets to
/// check the callback against and finish the exchange with.
public struct AuthorizationRequest: Sendable, Hashable {
    public var url: URL
    public var state: String
    public var verifier: String
    public var redirectURI: String
    public var clientID: String
}

public enum AuthError: Error, Sendable, Equatable {
    /// No tokens, or the grant is gone (`invalid_grant`): sign in again.
    case signedOut
    /// The user closed the sign-in sheet.
    case cancelled
    /// The server's RFC 6749 error code and description.
    case oauth(error: String, description: String?)
    /// The callback's `state` is not the one we sent.
    case stateMismatch
    /// The callback's `iss` is not the server we asked (RFC 9207).
    case issuerMismatch
    /// The server doesn't support what a native public client needs.
    case unsupportedServer(String)
    /// 429 `slow_down`: retry later.
    case rateLimited
}
