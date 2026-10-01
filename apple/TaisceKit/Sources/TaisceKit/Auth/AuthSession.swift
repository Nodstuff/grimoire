import Foundation

/// Shows the server's sign-in page and returns the redirect it ends on.
/// The app implements it with `ASWebAuthenticationSession` (shared, not
/// ephemeral, so iCloud Keychain passkeys work); tests script it.
public protocol WebAuthenticator: Sendable {
    func authenticate(url: URL, callbackScheme: String) async throws -> URL
}

/// One server's sign-in state and the `TokenProvider` every request goes
/// through. Refresh is single-flight: the server rotates the refresh token
/// on every use and revokes the whole grant when a spent one comes back
/// (outside a 60 s grace), so two concurrent refreshes would sign us out.
/// Every caller that needs a refresh joins the one in flight.
public actor AuthSession: TokenProvider {
    public enum State: Sendable, Hashable {
        case signedOut, signedIn
    }

    public nonisolated let oauth: OAuthClient
    let store: any TokenStore
    let now: @Sendable () -> Date
    /// Refresh this long before the access token expires.
    let refreshMargin: TimeInterval

    private var discovery: OAuthDiscovery?
    private var tokens: TokenSet?
    private var loaded = false
    private var refreshing: Task<TokenSet, any Error>?
    /// Bumped by sign-in/out so a refresh that lands afterwards is dropped.
    private var generation = 0
    private var watchers: [UUID: AsyncStream<State>.Continuation] = [:]

    public init(
        oauth: OAuthClient,
        store: any TokenStore,
        refreshMargin: TimeInterval = 120,
        now: @escaping @Sendable () -> Date = { .now }
    ) {
        self.oauth = oauth
        self.store = store
        self.refreshMargin = refreshMargin
        self.now = now
    }

    var server: String { oauth.origin }

    public var state: State { (try? current()) != nil ? .signedIn : .signedOut }

    /// The current state, then every change (a refresh that hits
    /// `invalid_grant` signs out from under the UI).
    public func states() -> AsyncStream<State> {
        let (stream, continuation) = AsyncStream<State>.makeStream(bufferingPolicy: .bufferingNewest(4))
        let id = UUID()
        watchers[id] = continuation
        continuation.yield(state)
        continuation.onTermination = { _ in Task { await self.unwatch(id) } }
        return stream
    }

    private func unwatch(_ id: UUID) { watchers[id] = nil }

    private func publish() {
        let s = state
        for c in watchers.values { c.yield(s) }
    }

    // MARK: TokenProvider

    public func token() async throws -> String? {
        guard let t = try current() else { throw AuthError.signedOut }
        if t.expiresAt.timeIntervalSince(now()) > refreshMargin { return t.accessToken }
        return try await refresh(from: t).accessToken
    }

    public func renew(rejected: String) async throws -> String? {
        guard let t = try current() else { throw AuthError.signedOut }
        // another request already refreshed past the rejected token
        if t.accessToken != rejected { return t.accessToken }
        return try await refresh(from: t).accessToken
    }

    // MARK: sign in / out

    /// Is this a server-mode daemon? A LOCAL-mode one serves no OAuth
    /// metadata and needs no token (loopback is trusted).
    public func requiresAuth() async throws -> Bool {
        if discovery != nil { return true }
        guard let d = try await oauth.discover() else { return false }
        discovery = d
        return true
    }

    /// Register (once per server), run the browser sign-in, exchange the code.
    public func signIn(using web: any WebAuthenticator) async throws {
        let d = try await discovered()
        let clientID = try await registeredClient(d)
        let req = try oauth.authorizationRequest(d, clientID: clientID)
        let callback: URL
        do {
            callback = try await web.authenticate(url: req.url, callbackScheme: OAuthClient.callbackScheme)
        } catch is CancellationError {
            throw AuthError.cancelled
        }
        let code = try oauth.code(from: callback, for: req, d)
        do {
            let t = try await oauth.exchange(code: code, for: req, d, now: now())
            generation += 1
            try save(t)
        } catch AuthError.oauth(error: "invalid_client", _) {
            try store.setClientID(nil, for: server)
            throw AuthError.oauth(error: "invalid_client", description: "the server forgot this app; sign in again")
        }
    }

    /// Revoke the grant (best effort: offline still signs out locally) and
    /// clear the tokens. The client id stays for the next sign-in.
    public func signOut() async {
        generation += 1
        refreshing = nil
        let t = try? current()
        try? save(nil)
        guard let t, let clientID = try? store.clientID(for: server), let d = try? await discovered() else { return }
        // revoking the refresh token revokes the whole grant server-side
        try? await oauth.revoke(t.refreshToken, clientID: clientID, d)
    }

    // MARK: internals

    func current() throws -> TokenSet? {
        if !loaded {
            tokens = try store.tokens(for: server)
            loaded = true
        }
        return tokens
    }

    private func save(_ t: TokenSet?) throws {
        let was = state
        try store.setTokens(t, for: server)
        tokens = t
        loaded = true
        if state != was { publish() }
    }

    func discovered() async throws -> OAuthDiscovery {
        if let discovery { return discovery }
        guard let d = try await oauth.discover() else {
            throw AuthError.unsupportedServer("this server does not use sign-in")
        }
        discovery = d
        return d
    }

    /// The cached DCR client id, or a fresh registration. A cached id the
    /// server no longer knows (a wiped server) is replaced.
    func registeredClient(_ d: OAuthDiscovery) async throws -> String {
        if let id = try store.clientID(for: server) {
            if await oauth.clientIsKnown(id, d) != false { return id }
        }
        let id = try await oauth.register(d)
        try store.setClientID(id, for: server)
        return id
    }

    private func refresh(from t: TokenSet) async throws -> TokenSet {
        if let inFlight = refreshing { return try await inFlight.value }
        // unstructured on purpose: a cancelled caller must not abandon a
        // rotation the server has already done (we'd lose the new token)
        let task = Task { try await self.performRefresh(t) }
        refreshing = task
        defer { refreshing = nil }
        return try await task.value
    }

    private func performRefresh(_ t: TokenSet) async throws -> TokenSet {
        guard let clientID = try store.clientID(for: server) else {
            try save(nil)
            throw AuthError.signedOut
        }
        let d = try await discovered()
        let gen = generation
        do {
            let next = try await oauth.refresh(t.refreshToken, clientID: clientID, d, now: now())
            guard gen == generation else { throw AuthError.signedOut }
            try save(next)
            return next
        } catch let AuthError.oauth(error, _) where error == "invalid_grant" || error == "invalid_client" {
            // revoked, expired, or reused: only a new sign-in helps
            guard gen == generation else { throw AuthError.signedOut }
            if error == "invalid_client" { try? store.setClientID(nil, for: server) }
            try save(nil)
            throw AuthError.signedOut
        }
    }
}
