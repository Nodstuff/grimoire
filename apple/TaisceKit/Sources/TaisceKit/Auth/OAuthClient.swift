import Foundation

/// The OAuth 2.1 calls a native public client makes against the daemon's
/// server mode (crates/daemon/src/auth/oauth.rs): discovery, dynamic client
/// registration, authorization code + PKCE S256, refresh, revoke. Stateless;
/// `AuthSession` owns the tokens and the single-flight refresh.
public struct OAuthClient: Sendable {
    /// Allowed by every server (`FIXED_REDIRECTS`) without configuration.
    public static let redirectURI = "ie.null.taisce:/oauth/callback"
    public static let callbackScheme = "ie.null.taisce"

    public let baseURL: URL
    public var clientName: String
    let session: URLSession

    public init(baseURL: URL, clientName: String = "Taisce iOS", session: URLSession = .shared) {
        self.baseURL = baseURL
        self.clientName = clientName
        self.session = session
    }

    /// The origin, no trailing slash: the key for stored tokens and what the
    /// server calls its `base`.
    public var origin: String {
        var s = baseURL.absoluteString
        while s.hasSuffix("/") { s.removeLast() }
        return s
    }

    // MARK: discovery

    /// Both metadata documents, or nil when the server does no OAuth (a
    /// LOCAL-mode daemon answers the well-known path with its SPA or a 404).
    public func discover() async throws -> OAuthDiscovery? {
        let prmURL = try endpoint("/.well-known/oauth-protected-resource")
        let (data, response) = try await session.data(for: jsonRequest(prmURL))
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        if status == 404 || response.mimeType == "text/html" { return nil }
        guard (200..<300).contains(status) else { throw APIError.http(status: status) }
        let prm: ProtectedResourceMetadata = try decode(data)

        let issuer = prm.authorizationServers.first ?? origin
        guard let issuerURL = URL(string: issuer) else { throw APIError.badURL(issuer) }
        let asURL = issuerURL.appending(path: ".well-known/oauth-authorization-server")
        let (asData, asResponse) = try await session.data(for: jsonRequest(asURL))
        let asStatus = (asResponse as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(asStatus) else { throw APIError.http(status: asStatus) }
        let meta: AuthorizationServerMetadata = try decode(asData)
        guard Self.trimmed(meta.issuer) == Self.trimmed(issuer) else { throw AuthError.issuerMismatch }
        guard meta.codeChallengeMethodsSupported?.contains("S256") ?? false else {
            throw AuthError.unsupportedServer("no PKCE S256")
        }
        if let methods = meta.tokenEndpointAuthMethodsSupported, !methods.contains("none") {
            throw AuthError.unsupportedServer("no public clients (token_endpoint_auth_method none)")
        }
        return OAuthDiscovery(resource: prm, server: meta)
    }

    // MARK: registration

    /// RFC 7591: register this app as a public client. Done once per server;
    /// the caller persists the `client_id`.
    public func register(_ d: OAuthDiscovery) async throws -> String {
        guard let url = d.server.registrationEndpoint else {
            throw AuthError.unsupportedServer("no registration_endpoint")
        }
        struct Body: Encodable {
            var client_name: String
            var redirect_uris: [String]
            var grant_types = ["authorization_code", "refresh_token"]
            var response_types = ["code"]
            var token_endpoint_auth_method = "none"
        }
        struct Registered: Decodable { var client_id: String }
        var r = jsonRequest(url, method: "POST")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(Body(client_name: clientName, redirect_uris: [Self.redirectURI]))
        let data = try await sendOAuth(r)
        let reg: Registered = try decode(data)
        return reg.client_id
    }

    /// Does the server still know `clientID` (a wiped hub forgets DCR
    /// clients)? The authorize endpoint shows an error page for an unknown
    /// client but redirects a known one with `invalid_request` when PKCE is
    /// missing; nothing is created either way. Nil = can't tell (offline,
    /// rate-limited), so keep the cached id.
    public func clientIsKnown(_ clientID: String, _ d: OAuthDiscovery) async -> Bool? {
        guard var c = URLComponents(url: d.server.authorizationEndpoint, resolvingAgainstBaseURL: false) else { return nil }
        c.queryItems = [
            URLQueryItem(name: "client_id", value: clientID),
            URLQueryItem(name: "redirect_uri", value: Self.redirectURI),
            URLQueryItem(name: "response_type", value: "code"),
        ]
        guard let url = c.url,
              let (_, response) = try? await session.data(for: URLRequest(url: url), delegate: NoRedirects()),
              let http = response as? HTTPURLResponse
        else { return nil }
        switch http.statusCode {
        case 300..<400:
            return http.value(forHTTPHeaderField: "Location")?.hasPrefix(Self.redirectURI) == true ? true : nil
        case 400: return false
        default: return nil
        }
    }

    // MARK: authorization code + PKCE

    public func authorizationRequest(_ d: OAuthDiscovery, clientID: String, pkce: PKCE = PKCE(), state: String = PKCE.randomURLSafe(bytes: 24)) throws -> AuthorizationRequest {
        guard var c = URLComponents(url: d.server.authorizationEndpoint, resolvingAgainstBaseURL: false) else {
            throw APIError.badURL(d.server.authorizationEndpoint.absoluteString)
        }
        var q = [
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "client_id", value: clientID),
            URLQueryItem(name: "redirect_uri", value: Self.redirectURI),
            URLQueryItem(name: "state", value: state),
            URLQueryItem(name: "code_challenge", value: pkce.challenge),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
            URLQueryItem(name: "resource", value: d.resource.resource),
        ]
        if let scopes = d.resource.scopesSupported, !scopes.isEmpty {
            q.append(URLQueryItem(name: "scope", value: scopes.joined(separator: " ")))
        }
        c.queryItems = q
        guard let url = c.url else { throw APIError.badURL("authorize") }
        return AuthorizationRequest(url: url, state: state, verifier: pkce.verifier, redirectURI: Self.redirectURI, clientID: clientID)
    }

    /// The code from the redirect, after checking `state` and `iss`.
    public func code(from callback: URL, for req: AuthorizationRequest, _ d: OAuthDiscovery) throws -> String {
        let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
        func value(_ name: String) -> String? { items.first { $0.name == name }?.value }
        guard value("state") == req.state else { throw AuthError.stateMismatch }
        if let iss = value("iss"), Self.trimmed(iss) != Self.trimmed(d.server.issuer) {
            throw AuthError.issuerMismatch
        }
        if let error = value("error") {
            if error == "access_denied" { throw AuthError.cancelled }
            throw AuthError.oauth(error: error, description: value("error_description"))
        }
        guard let code = value("code"), !code.isEmpty else {
            throw AuthError.oauth(error: "invalid_request", description: "callback carries no code")
        }
        return code
    }

    public func exchange(code: String, for req: AuthorizationRequest, _ d: OAuthDiscovery, now: Date = .now) async throws -> TokenSet {
        try await tokenRequest(d, now: now, previousRefresh: nil, form: [
            ("grant_type", "authorization_code"),
            ("code", code),
            ("code_verifier", req.verifier),
            ("client_id", req.clientID),
            ("redirect_uri", req.redirectURI),
            ("resource", d.resource.resource),
        ])
    }

    /// Rotate: the old refresh token is spent; persist the returned one
    /// before anything else uses the session.
    public func refresh(_ refreshToken: String, clientID: String, _ d: OAuthDiscovery, now: Date = .now) async throws -> TokenSet {
        try await tokenRequest(d, now: now, previousRefresh: refreshToken, form: [
            ("grant_type", "refresh_token"),
            ("refresh_token", refreshToken),
            ("client_id", clientID),
            ("resource", d.resource.resource),
        ])
    }

    /// RFC 7009. The server answers 200 whether or not the token was live.
    public func revoke(_ token: String, clientID: String, _ d: OAuthDiscovery) async throws {
        guard let url = d.server.revocationEndpoint else { return }
        _ = try await sendOAuth(formRequest(url, [("token", token), ("client_id", clientID)]))
    }

    // MARK: plumbing

    func tokenRequest(_ d: OAuthDiscovery, now: Date, previousRefresh: String?, form: [(String, String)]) async throws -> TokenSet {
        let data = try await sendOAuth(formRequest(d.server.tokenEndpoint, form))
        let t: TokenResponse = try decode(data)
        guard t.tokenType.caseInsensitiveCompare("bearer") == .orderedSame else {
            throw AuthError.unsupportedServer("token_type \(t.tokenType)")
        }
        guard let refresh = t.refreshToken ?? previousRefresh else {
            throw AuthError.unsupportedServer("no refresh_token")
        }
        return TokenSet(
            accessToken: t.accessToken,
            refreshToken: refresh,
            expiresAt: now.addingTimeInterval(TimeInterval(t.expiresIn ?? 3600)),
            scope: t.scope
        )
    }

    /// Sends an OAuth endpoint request; RFC 6749 §5.2 error bodies become
    /// `AuthError.oauth`, 429 `rateLimited`.
    func sendOAuth(_ r: URLRequest) async throws -> Data {
        let (data, response) = try await session.data(for: r)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        if (200..<300).contains(status) { return data }
        if status == 429 { throw AuthError.rateLimited }
        struct ErrorBody: Decodable { var error: String; var error_description: String? }
        if let e = try? JSONDecoder().decode(ErrorBody.self, from: data) {
            throw AuthError.oauth(error: e.error, description: e.error_description)
        }
        throw APIError.http(status: status)
    }

    func endpoint(_ path: String) throws -> URL {
        guard let u = URL(string: origin + path) else { throw APIError.badURL(path) }
        return u
    }

    func jsonRequest(_ url: URL, method: String = "GET") -> URLRequest {
        var r = URLRequest(url: url)
        r.httpMethod = method
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        return r
    }

    func formRequest(_ url: URL, _ form: [(String, String)]) -> URLRequest {
        var r = jsonRequest(url, method: "POST")
        r.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "Content-Type")
        r.httpBody = Data(Self.formEncode(form).utf8)
        return r
    }

    /// application/x-www-form-urlencoded with everything but RFC 3986
    /// unreserved characters escaped (tokens are base64url, but a `+` in
    /// anything must never decode as a space).
    static func formEncode(_ pairs: [(String, String)]) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        func esc(_ s: String) -> String { s.addingPercentEncoding(withAllowedCharacters: allowed) ?? s }
        return pairs.map { "\(esc($0.0))=\(esc($0.1))" }.joined(separator: "&")
    }

    static func trimmed(_ s: String) -> String {
        var s = s
        while s.hasSuffix("/") { s.removeLast() }
        return s
    }

    func decode<T: Decodable>(_ data: Data) throws -> T {
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw APIError.decoding(String(describing: error))
        }
    }
}

/// Stops URLSession following a redirect (the probe's target is our own
/// custom scheme, which URLSession can't load).
final class NoRedirects: NSObject, URLSessionTaskDelegate {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest) async -> URLRequest? {
        nil
    }
}
