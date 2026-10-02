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
