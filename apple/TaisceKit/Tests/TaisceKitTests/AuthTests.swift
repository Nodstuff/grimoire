import Foundation
import Synchronization
import Testing
@testable import TaisceKit

/// A scripted server-mode daemon: discovery, DCR, the authorize probe,
/// rotating refresh with reuse detection, and a bearer-guarded /api.
final class FakeAuthServer: Sendable {
    struct State {
        var registrations = 0
        var knownClients: Set<String> = []
        var tokenForms: [[String: String]] = []
        var revoked: [String] = []
        var challenge: String?
        var access = "a0"
        var refresh = "r0"
        var issued = 0
        var apiCalls = 0
        var refreshDelay: TimeInterval = 0.05
        /// GET /.well-known answers with the SPA, as a LOCAL-mode daemon does
        var localMode = false
        /// the server takes the universal-link redirect (this build's server);
        /// false = an older one, which refuses it at registration
        var appLinks = false
        /// redirects per client beyond the custom scheme (default: custom only)
        var clientRedirects: [String: [String]] = [:]
        /// every registration's redirect_uris
        var registered: [[String]] = []
        /// the callback each sign-in was told to wait for
        var callbacks: [OAuthCallback] = []
    }

    final class Box: Sendable {
        let m: Mutex<State>
        init(_ s: State) { m = Mutex(s) }
        func withLock<R>(_ body: (inout State) -> R) -> R { m.withLock { body(&$0) } }
    }

    let state: Box
    let server: MockServer

    static let base = "http://mock.local"

    init(_ configure: (inout State) -> Void = { _ in }) {
        var s = State()
        configure(&s)
        let box = Box(s)
        state = box
        server = MockServer { r in Self.handle(r, box) }
    }

    var snapshot: State { state.withLock { $0 } }

    func oauth() -> OAuthClient {
        OAuthClient(baseURL: URL(string: Self.base)!, session: server.session)
    }

    /// As the release app sees taisce.null.ie: an https origin whose host
    /// the build claims (the mock answers any URL the session sends).
    func appLinkOAuth(available: Bool = true) -> OAuthClient {
        OAuthClient(baseURL: URL(string: Self.httpsBase)!, appLinkHosts: ["mock.local"], httpsCallbackAvailable: available, session: server.session)
    }

    static let httpsBase = "https://mock.local"

    func api(_ provider: any TokenProvider) -> APIClient {
        APIClient(config: ServerConfig(baseURL: URL(string: Self.base)!, tokenProvider: provider), session: server.session)
    }

    static func form(_ r: URLRequest) -> [String: String] {
        let body = r.httpBody.flatMap { String(data: $0, encoding: .utf8) } ?? ""
        var c = URLComponents()
        c.percentEncodedQuery = body
        return Dictionary((c.queryItems ?? []).map { ($0.name, $0.value ?? "") }, uniquingKeysWith: { _, b in b })
    }

    static func oauthError(_ code: String, status: Int = 400) -> MockServer.Reply {
        MockServer.Reply(status: status, chunks: [Data(#"{"error":"\#(code)","error_description":"x"}"#.utf8)])
    }

    static func handle(_ r: URLRequest, _ state: Box) -> MockServer.Reply {
        var snapshot: State { state.withLock { $0 } }
        let b = Self.base
        switch (r.httpMethod ?? "GET", r.path) {
        case ("GET", "/.well-known/oauth-protected-resource"):
            if snapshot.localMode {
                return MockServer.Reply(chunks: [Data("<!doctype html>".utf8)], contentType: "text/html")
            }
            return .json(#"{"resource":"\#(b)","authorization_servers":["\#(b)"],"bearer_methods_supported":["header"],"scopes_supported":["grimoire"],"resource_name":"Taisce"}"#)
        case ("GET", "/.well-known/oauth-authorization-server"):
            return .json("""
            {"issuer":"\(b)","authorization_endpoint":"\(b)/oauth/authorize","token_endpoint":"\(b)/oauth/token",\
            "registration_endpoint":"\(b)/oauth/register","revocation_endpoint":"\(b)/oauth/revoke",\
            "scopes_supported":["grimoire"],"response_types_supported":["code"],"response_modes_supported":["query"],\
            "grant_types_supported":["authorization_code","refresh_token"],"token_endpoint_auth_methods_supported":["none"],\
            "revocation_endpoint_auth_methods_supported":["none"],"code_challenge_methods_supported":["S256"],\
            "client_id_metadata_document_supported":true,"authorization_response_iss_parameter_supported":true}
            """)
        case ("POST", "/oauth/register"):
            struct Body: Decodable { var redirect_uris: [String] }
            let uris = (r.httpBody.flatMap { try? JSONDecoder().decode(Body.self, from: $0) })?.redirect_uris ?? []
            let link = uris.first { $0.hasPrefix("https://") }
            let reply: MockServer.Reply = state.withLock { s in
                s.registered.append(uris)
                if let link {
                    // an older server: not an allowed redirect; this one: the pinned app client
                    guard s.appLinks else { return Self.oauthError("invalid_redirect_uri") }
                    s.knownClients.insert("taisce-app")
                    s.clientRedirects["taisce-app"] = [OAuthClient.redirectURI, link]
                    return MockServer.Reply(status: 201, chunks: [Data(#"{"client_id":"taisce-app","redirect_uris":["\#(OAuthClient.redirectURI)","\#(link)"]}"#.utf8)])
                }
                s.registrations += 1
                let id = "dcr_\(s.registrations)"
                s.knownClients.insert(id)
                return MockServer.Reply(status: 201, chunks: [Data(#"{"client_id":"\#(id)","token_endpoint_auth_method":"none"}"#.utf8)])
            }
            return reply
        case ("GET", "/oauth/authorize"):
            // the probe: known client → redirect with invalid_request (no PKCE)
            let id = r.query["client_id"] ?? ""
            let redirect = r.query["redirect_uri"] ?? OAuthClient.redirectURI
            guard snapshot.knownClients.contains(id), (snapshot.clientRedirects[id] ?? [OAuthClient.redirectURI]).contains(redirect) else {
                return MockServer.Reply(status: 400, chunks: [Data("<p>Unknown client</p>".utf8)], contentType: "text/html")
            }
            return MockServer.Reply(status: 302, chunks: [], headers: ["Location": "\(redirect)?error=invalid_request"])
        case ("POST", "/oauth/token"):
            return token(Self.form(r), state)
        case ("POST", "/oauth/revoke"):
            state.withLock { $0.revoked.append(Self.form(r)["token"] ?? "") }
            return MockServer.Reply(chunks: [])
        case ("GET", let p) where p.hasPrefix("/api/"):
            let ok = state.withLock { s in
                s.apiCalls += 1
                return r.value(forHTTPHeaderField: "Authorization") == "Bearer \(s.access)"
            }
            guard ok else {
                return MockServer.Reply(status: 401, chunks: [Data(#"{"error":"invalid_token","code":"unauthorized"}"#.utf8)],
                                        headers: ["WWW-Authenticate": #"Bearer resource_metadata="\#(b)/.well-known/oauth-protected-resource""#])
            }
            if p == "/api/changes/stream" { return .sse("retry: 3000\n\n", ": ping\n\n") }
            return .json("[\(Fixture.summary("d1", title: "A"))]")
        default:
            return MockServer.Reply(status: 404, chunks: [])
        }
    }

    static func token(_ f: [String: String], _ state: Box) -> MockServer.Reply {
        let delay = state.withLock { s in
            s.tokenForms.append(f)
            return s.refreshDelay
        }
        // widen the race window so concurrent refreshes would overlap
        Thread.sleep(forTimeInterval: delay)
        return state.withLock { s -> MockServer.Reply in
            switch f["grant_type"] {
            case "authorization_code":
                guard f["code"] == "code1", let v = f["code_verifier"], PKCE.challenge(for: v) == s.challenge else {
                    return Self.oauthError("invalid_grant")
                }
            case "refresh_token":
                // rotation with reuse detection: a spent token revokes everything
                guard f["refresh_token"] == s.refresh else {
                    s.refresh = "revoked"
                    s.access = "revoked"
                    return Self.oauthError("invalid_grant")
                }
            default:
                return Self.oauthError("unsupported_grant_type")
            }
            s.issued += 1
            s.access = "a\(s.issued)"
            s.refresh = "r\(s.issued)"
            return .json(#"{"access_token":"\#(s.access)","token_type":"Bearer","expires_in":3600,"refresh_token":"\#(s.refresh)","scope":"grimoire"}"#)
        }
    }

    var tokenCalls: Int { snapshot.tokenForms.count }
}

/// Plays the browser: records the authorize URL, "signs in", and redirects.
struct ScriptedWeb: WebAuthenticator {
    var server: FakeAuthServer
    var mangle: @Sendable (inout [URLQueryItem]) -> Void = { _ in }

    func authenticate(url: URL, callback: OAuthCallback) async throws -> URL {
        let q = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        server.state.withLock {
            $0.challenge = q.first { $0.name == "code_challenge" }?.value
            $0.callbacks.append(callback)
        }
        var c = URLComponents(string: callback.redirectURI)!
        var items = [
            URLQueryItem(name: "code", value: "code1"),
            URLQueryItem(name: "iss", value: FakeAuthServer.base),
            URLQueryItem(name: "state", value: q.first { $0.name == "state" }?.value),
        ]
        mangle(&items)
        c.queryItems = items
        return c.url!
    }
}

@Suite struct AuthTests {
    static let server = FakeAuthServer.base
    final class Clock: Sendable {
        let m = Mutex(Date(timeIntervalSince1970: 1_800_000_000))
        func withLock<R>(_ body: (inout Date) -> R) -> R { m.withLock { body(&$0) } }
    }

    let clock = Clock()

    func session(_ fake: FakeAuthServer, store: MemoryTokenStore) -> AuthSession {
        let clock = self.clock
        return AuthSession(oauth: fake.oauth(), store: store, now: { clock.withLock { $0 } })
    }

    func signedIn(expiresIn: TimeInterval = 3600) -> MemoryTokenStore {
        let now = clock.withLock { $0 }
        return MemoryTokenStore(
            tokens: [Self.server: TokenSet(accessToken: "a0", refreshToken: "r0", expiresAt: now.addingTimeInterval(expiresIn))],
            clients: [Self.server: "dcr_0"]
        )
    }

    // MARK: PKCE + encoding

    @Test func pkceMatchesRFC7636() {
        #expect(PKCE.challenge(for: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk") == "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM")
        let p = PKCE()
        #expect(p.verifier.count == 43)
        #expect(p.verifier.allSatisfy { $0.isASCII && ($0.isLetter || $0.isNumber || $0 == "-" || $0 == "_") })
        #expect(PKCE().verifier != p.verifier)
    }

    @Test func formEncodingEscapesEverythingReserved() {
        #expect(OAuthClient.formEncode([("a", "x+y z/="), ("b", "ie.null.taisce:/oauth/callback")])
            == "a=x%2By%20z%2F%3D&b=ie.null.taisce%3A%2Foauth%2Fcallback")
    }

    // MARK: discovery

    @Test func discoveryDecodesTheServersDocuments() async throws {
        let fake = FakeAuthServer()
        let d = try #require(try await fake.oauth().discover())
        #expect(d.resource.resource == Self.server)
        #expect(d.server.tokenEndpoint.absoluteString == "\(Self.server)/oauth/token")
        #expect(d.server.codeChallengeMethodsSupported == ["S256"])
        #expect(d.server.authorizationResponseIssParameterSupported == true)
    }

    @Test func localModeServerNeedsNoAuth() async throws {
        let fake = FakeAuthServer { $0.localMode = true }
        #expect(try await fake.oauth().discover() == nil)
        #expect(try await session(fake, store: MemoryTokenStore()).requiresAuth() == false)
    }

    // MARK: sign in

    @Test func signInRegistersOnceThenExchangesTheCode() async throws {
        let fake = FakeAuthServer()
        let store = MemoryTokenStore()
        let auth = session(fake, store: store)
        #expect(await auth.state == .signedOut)
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(await auth.state == .signedIn)
        #expect(try store.clientID(for: Self.server) == "dcr_1")
        #expect(try store.tokens(for: Self.server)?.accessToken == "a1")

        let f = try #require(fake.snapshot.tokenForms.first)
        #expect(f["grant_type"] == "authorization_code")
        #expect(f["code"] == "code1")
        #expect(f["client_id"] == "dcr_1")
        #expect(f["redirect_uri"] == "ie.null.taisce:/oauth/callback")
        #expect(f["resource"] == Self.server)
        #expect(f["code_verifier"]?.count == 43)

        // sign out and in again: the cached client is probed, not re-registered
        await auth.signOut()
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(fake.snapshot.registrations == 1)
        #expect(fake.server.requests.contains { $0.path == "/oauth/authorize" && $0.query["client_id"] == "dcr_1" })
    }

    // MARK: the universal-link redirect (ADR 0004 follow-up)

    func appLinkSession(_ fake: FakeAuthServer, store: MemoryTokenStore, available: Bool = true) -> AuthSession {
        let clock = self.clock
        return AuthSession(oauth: fake.appLinkOAuth(available: available), store: store, now: { clock.withLock { $0 } })
    }

    static let appLink = "https://mock.local/oauth/app-callback"

    @Test func aNewServerSignsInThroughTheUniversalLink() async throws {
        let fake = FakeAuthServer { $0.appLinks = true }
        let store = MemoryTokenStore()
        let auth = appLinkSession(fake, store: store)
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(await auth.state == .signedIn)
        // registered with the https redirect first, beside the custom scheme
        #expect(fake.snapshot.registered == [[Self.appLink, OAuthClient.redirectURI]])
        #expect(try store.clientID(for: FakeAuthServer.httpsBase) == "taisce-app")
        #expect(fake.snapshot.callbacks == [.https(host: "mock.local", path: "/oauth/app-callback")])
        let f = try #require(fake.snapshot.tokenForms.first)
        #expect(f["redirect_uri"] == Self.appLink, "the code is exchanged at the redirect it came back on")
        // next sign-in: the cached client holds the link, so no new registration
        await auth.signOut()
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(fake.snapshot.registered.count == 1)
        #expect(fake.snapshot.callbacks.last == .https(host: "mock.local", path: "/oauth/app-callback"))
    }

    @Test func anOlderServerKeepsTheCustomScheme() async throws {
        // the server deployed before this build refuses the https redirect
        let fake = FakeAuthServer { $0.appLinks = false }
        let store = MemoryTokenStore()
        let auth = appLinkSession(fake, store: store)
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(await auth.state == .signedIn)
        #expect(fake.snapshot.registered == [[Self.appLink, OAuthClient.redirectURI], [OAuthClient.redirectURI]])
        #expect(try store.clientID(for: FakeAuthServer.httpsBase) == "dcr_1")
        #expect(fake.snapshot.callbacks == [.custom])
        #expect(fake.snapshot.tokenForms.first?["redirect_uri"] == OAuthClient.redirectURI)
    }

    @Test func aCachedCustomSchemeClientMovesToTheLinkOnlyAtSignIn() async throws {
        // a device signed in with the custom scheme keeps its tokens and
        // client: nothing re-registers until it signs in again
        let fake = FakeAuthServer {
            $0.appLinks = true
            $0.knownClients = ["dcr_0"]
        }
        let now = clock.withLock { $0 }
        let store = MemoryTokenStore(
            tokens: [FakeAuthServer.httpsBase: TokenSet(accessToken: "a0", refreshToken: "r0", expiresAt: now.addingTimeInterval(3600))],
            clients: [FakeAuthServer.httpsBase: "dcr_0"]
        )
        let auth = appLinkSession(fake, store: store)
        #expect(await auth.state == .signedIn)
        #expect(try await auth.token() == "a0")
        #expect(fake.snapshot.registered.isEmpty && fake.snapshot.tokenForms.isEmpty)
        // a later sign-in: dcr_0 doesn't hold the link, so the app registers it
        await auth.signOut()
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(try store.clientID(for: FakeAuthServer.httpsBase) == "taisce-app")
        #expect(fake.snapshot.callbacks == [.https(host: "mock.local", path: "/oauth/app-callback")])
    }

    @Test func withoutTheOSCallbackTheCustomSchemeIsUsed() async throws {
        let fake = FakeAuthServer { $0.appLinks = true }
        let auth = appLinkSession(fake, store: MemoryTokenStore(), available: false)
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(fake.snapshot.registered == [[OAuthClient.redirectURI]])
        #expect(fake.snapshot.callbacks == [.custom])
    }

    @Test func redirectSelectionByOSVersionAndServer() {
        let v = { (major: Int, minor: Int) in OperatingSystemVersion(majorVersion: major, minorVersion: minor, patchVersion: 0) }
        // iOS 17.4 and macOS 14.4 brought ASWebAuthenticationSession's https callback
        #expect(!OAuthCallback.httpsSupported(v(17, 3), mac: false))
        #expect(OAuthCallback.httpsSupported(v(17, 4), mac: false))
        #expect(OAuthCallback.httpsSupported(v(26, 0), mac: false))
        #expect(!OAuthCallback.httpsSupported(v(14, 3), mac: true))
        #expect(OAuthCallback.httpsSupported(v(14, 4), mac: true))
        #expect(OAuthCallback.httpsSupported(v(26, 0), mac: true))
        // this build's targets are above both floors
        #expect(OAuthCallback.httpsSupported(ProcessInfo.processInfo.operatingSystemVersion, mac: OAuthCallback.runningOnMac))

        let hosts: Set<String> = ["taisce.null.ie"]
        let prod = URL(string: "https://taisce.null.ie")!
        #expect(OAuthCallback.appLink(for: prod, claimedHosts: hosts, available: true) == .https(host: "taisce.null.ie", path: "/oauth/app-callback"))
        #expect(OAuthCallback.appLink(for: prod, claimedHosts: hosts, available: true)?.redirectURI == "https://taisce.null.ie/oauth/app-callback")
        #expect(OAuthCallback.appLink(for: prod, claimedHosts: hosts, available: false) == nil, "an older OS")
        #expect(OAuthCallback.appLink(for: prod, claimedHosts: [], available: true) == nil, "a build without the entitlement (Debug)")
        #expect(OAuthCallback.appLink(for: URL(string: "http://127.0.0.1:7531")!, claimedHosts: ["127.0.0.1"], available: true) == nil, "plain http")
        #expect(OAuthCallback.appLink(for: URL(string: "https://other.example")!, claimedHosts: hosts, available: true) == nil, "another host")
        #expect(OAuthCallback.appLink(for: URL(string: "https://taisce.null.ie:8443")!, claimedHosts: hosts, available: true) == nil, "universal links are port 443")
        #expect(OAuthCallback.custom.redirectURI == "ie.null.taisce:/oauth/callback")
    }

    @Test func authorizationURLCarriesPKCEStateAndResource() async throws {
        let fake = FakeAuthServer()
        let oauth = fake.oauth()
        let d = try #require(try await oauth.discover())
        let req = try oauth.authorizationRequest(d, clientID: "dcr_9", pkce: PKCE(verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"), state: "st")
        let q = URLRequest(url: req.url).query
        #expect(req.url.path() == "/oauth/authorize")
        #expect(q == [
            "response_type": "code", "client_id": "dcr_9", "redirect_uri": "ie.null.taisce:/oauth/callback",
            "state": "st", "code_challenge": "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            "code_challenge_method": "S256", "resource": Self.server, "scope": "grimoire",
        ])
    }

    @Test func aForgottenClientIsRegisteredAgain() async throws {
        let fake = FakeAuthServer()
        let store = MemoryTokenStore(clients: [Self.server: "dcr_stale"])
        try await session(fake, store: store).signIn(using: ScriptedWeb(server: fake))
        #expect(fake.snapshot.registrations == 1)
        #expect(try store.clientID(for: Self.server) == "dcr_1")
    }

    @Test func callbackWithTheWrongStateOrIssuerIsRefused() async throws {
        let fake = FakeAuthServer()
        let store = MemoryTokenStore()
        let auth = session(fake, store: store)
        let badState = ScriptedWeb(server: fake) { items in
            items.removeAll { $0.name == "state" }
            items.append(URLQueryItem(name: "state", value: "forged"))
        }
        await #expect(throws: AuthError.stateMismatch) { try await auth.signIn(using: badState) }
        let badIss = ScriptedWeb(server: fake) { items in
            items.removeAll { $0.name == "iss" }
            items.append(URLQueryItem(name: "iss", value: "https://evil.example"))
        }
        await #expect(throws: AuthError.issuerMismatch) { try await auth.signIn(using: badIss) }
        let denied = ScriptedWeb(server: fake) { items in
            items.removeAll { $0.name == "code" }
            items.append(URLQueryItem(name: "error", value: "access_denied"))
        }
        await #expect(throws: AuthError.cancelled) { try await auth.signIn(using: denied) }
        #expect(fake.tokenCalls == 0)
        #expect(await auth.state == .signedOut)
    }

    @Test func aCancelledSignInLeavesAUsableStateAndCanRetry() async throws {
        let fake = FakeAuthServer()
        let store = MemoryTokenStore()
        let auth = session(fake, store: store)
        await #expect(throws: AuthError.cancelled) { try await auth.signIn(using: CancellingWeb()) }
        #expect(await auth.state == .signedOut)
        #expect(try store.tokens(for: Self.server) == nil)
        await #expect(throws: AuthError.signedOut) { try await auth.token() }
        // the retry reuses the client registered before the cancel
        try await auth.signIn(using: ScriptedWeb(server: fake))
        #expect(await auth.state == .signedIn && fake.snapshot.registrations == 1)
    }

    @Test func tokensSurviveARelaunch() async throws {
        let fake = FakeAuthServer()
        let store = MemoryTokenStore()
        try await session(fake, store: store).signIn(using: ScriptedWeb(server: fake))
        // a new process: a fresh session over the same store
        let relaunched = session(fake, store: store)
        #expect(await relaunched.state == .signedIn)
        #expect(try await relaunched.token() == "a1")
    }

    // MARK: refresh

    @Test func nearExpiryRefreshesAndRotates() async throws {
        let fake = FakeAuthServer()
        let store = signedIn(expiresIn: 60) // inside the 120 s margin
        let auth = session(fake, store: store)
        #expect(try await auth.token() == "a1")
        #expect(fake.snapshot.tokenForms.last?["refresh_token"] == "r0")
        #expect(fake.snapshot.tokenForms.last?["client_id"] == "dcr_0")
        #expect(try store.tokens(for: Self.server)?.refreshToken == "r1")
        // fresh now: no call
        #expect(try await auth.token() == "a1")
        #expect(fake.tokenCalls == 1)
        // an hour later the rotated token is the one sent
        clock.withLock { $0 += 3600 }
        #expect(try await auth.token() == "a2")
        #expect(fake.snapshot.tokenForms.last?["refresh_token"] == "r1")
    }

    @Test func concurrentCallersShareOneRefresh() async throws {
        let fake = FakeAuthServer()
        let auth = session(fake, store: signedIn(expiresIn: 10))
        let tokens = try await withThrowingTaskGroup(of: String?.self) { g in
            for _ in 0..<25 { g.addTask { try await auth.token() } }
            return try await g.reduce(into: [String?]()) { $0.append($1) }
        }
        #expect(tokens.count == 25 && tokens.allSatisfy { $0 == "a1" })
        #expect(fake.tokenCalls == 1)
    }

    @Test func a401RefreshesOnceAndRetries() async throws {
        let fake = FakeAuthServer { $0.access = "rotated-elsewhere" }
        let auth = session(fake, store: signedIn())
        let api = fake.api(auth)
        let results = try await withThrowingTaskGroup(of: Int.self) { g in
            for _ in 0..<10 { g.addTask { try await api.tree().count } }
            return try await g.reduce(into: [Int]()) { $0.append($1) }
        }
        #expect(results == Array(repeating: 1, count: 10))
        #expect(fake.tokenCalls == 1)
        #expect(try await auth.token() == "a1")
    }

    @Test func invalidGrantSignsOut() async throws {
        let fake = FakeAuthServer { $0.refresh = "someone-else-rotated" }
        let store = signedIn(expiresIn: 0)
        let auth = session(fake, store: store)
        var states = await auth.states().makeAsyncIterator()
        #expect(await states.next() == .signedIn)
        await #expect(throws: AuthError.signedOut) { _ = try await auth.token() }
        #expect(await states.next() == .signedOut)
        #expect(try store.tokens(for: Self.server) == nil)
        #expect(try store.clientID(for: Self.server) == "dcr_0")
        // and a request now fails fast, no network
        let before = fake.server.requests.count
        await #expect(throws: AuthError.signedOut) { _ = try await fake.api(auth).tree() }
        #expect(fake.server.requests.count == before)
    }

    @Test func unauthorizedAfterTheRetryIsReported() async throws {
        let fake = FakeAuthServer { $0.access = "never-matches" }
        // a provider that hands out the same token again
        struct Stubborn: TokenProvider {
            func token() async throws -> String? { "x" }
            func renew(rejected: String) async throws -> String? { "x" }
        }
        await #expect(throws: APIError.unauthorized) { _ = try await fake.api(Stubborn()).tree() }
        #expect(fake.snapshot.apiCalls == 2)
    }

    @Test func signOutRevokesAndKeepsTheClient() async throws {
        let fake = FakeAuthServer()
        let store = signedIn()
        let auth = session(fake, store: store)
        await auth.signOut()
        #expect(fake.snapshot.revoked == ["r0"])
        #expect(try store.tokens(for: Self.server) == nil)
        #expect(try store.clientID(for: Self.server) == "dcr_0")
        #expect(await auth.state == .signedOut)
    }

    // MARK: the stream

    @Test func streamSendsTheBearerAndRenewsOn401() async throws {
        let fake = FakeAuthServer { $0.access = "rotated-elsewhere" }
        let auth = session(fake, store: signedIn())
        var out: [SSEOutput] = []
        for try await o in fake.api(auth).changeStream(lastEventID: 3) { out.append(o) }
        #expect(!out.isEmpty)
        let streams = fake.server.requests.filter { $0.path == "/api/changes/stream" }
        #expect(streams.map { $0.value(forHTTPHeaderField: "Authorization") } == ["Bearer a0", "Bearer a1"])
        #expect(streams.allSatisfy { $0.value(forHTTPHeaderField: "Last-Event-ID") == "3" })
        #expect(fake.tokenCalls == 1)
    }
}

/// The user closed the sign-in sheet.
struct CancellingWeb: WebAuthenticator {
    func authenticate(url: URL, callback: OAuthCallback) async throws -> URL { throw AuthError.cancelled }
}
