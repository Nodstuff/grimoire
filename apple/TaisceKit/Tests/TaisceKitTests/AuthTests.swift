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
            let id = state.withLock { s in
                s.registrations += 1
                let id = "dcr_\(s.registrations)"
                s.knownClients.insert(id)
                return id
            }
            return MockServer.Reply(status: 201, chunks: [Data(#"{"client_id":"\#(id)","token_endpoint_auth_method":"none"}"#.utf8)])
        case ("GET", "/oauth/authorize"):
            // the probe: known client → redirect with invalid_request (no PKCE)
            guard snapshot.knownClients.contains(r.query["client_id"] ?? "") else {
                return MockServer.Reply(status: 400, chunks: [Data("<p>Unknown client</p>".utf8)], contentType: "text/html")
            }
            return MockServer.Reply(status: 302, chunks: [], headers: ["Location": "ie.null.taisce:/oauth/callback?error=invalid_request"])
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

    func authenticate(url: URL, callbackScheme: String) async throws -> URL {
        let q = URLComponents(url: url, resolvingAgainstBaseURL: false)?.queryItems ?? []
        server.state.withLock { $0.challenge = q.first { $0.name == "code_challenge" }?.value }
        var c = URLComponents(string: "\(callbackScheme):/oauth/callback")!
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
