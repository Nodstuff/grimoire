import Foundation
import TaisceKit
import Observation

/// App-wide state: the server connection, its sign-in, cache and sync
/// engine, and the doc tree the sidebar shows. Everything below the UI lives
/// in TaisceKit.
@MainActor @Observable
final class AppModel {
    static let serverURLKey = "serverURL"
    static let defaultServerURL = "https://taisce.null.ie"

    enum AuthPhase: Equatable {
        /// a loopback (LOCAL-mode) daemon: no tokens
        case notRequired
        case signedOut
        case signedIn
    }

    private(set) var serverURL: String
    private(set) var api: APIClient?
    private(set) var cache: Cache?
    private(set) var sync: SyncEngine?
    private(set) var auth: AuthSession?
    private(set) var authPhase: AuthPhase = .notRequired
    private(set) var isSigningIn = false
    private(set) var docs: [DocRecord] = []
    private(set) var tree: [DocTreeNode] = []
    var lastError: String?
    let dueAlerts = NotificationCoordinator()

    private var observeTask: Task<Void, Never>?
    private var authTask: Task<Void, Never>?

    init() {
        let stored = UserDefaults.standard.string(forKey: Self.serverURLKey)
        serverURL = stored.flatMap { ServerURLPolicy.accepts($0) ? $0 : nil } ?? Self.defaultServerURL
    }

    var needsSignIn: Bool { authPhase == .signedOut }

    func boot() async {
        guard api == nil else { return }
        await connect()
        await startSync()
    }

    func setServerURL(_ s: String) async {
        let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != serverURL else { return }
        if case .failure(let rejection) = ServerURLPolicy.check(trimmed) {
            lastError = rejection.message
            return
        }
        await stopSync()
        serverURL = trimmed
        UserDefaults.standard.set(trimmed, forKey: Self.serverURLKey)
        await connect()
        await startSync()
    }

    func startSync() async {
        guard authPhase != .signedOut else { return }
        await sync?.start()
        await dueAlerts.reconcile()
    }

    func stopSync() async { await sync?.stop() }

    // MARK: sign-in

    func signIn() async {
        guard let auth, !isSigningIn else { return }
        isSigningIn = true
        defer { isSigningIn = false }
        lastError = nil
        do {
            if try await !auth.requiresAuth() {
                // a LOCAL-mode daemon reached by a non-loopback name: no tokens
                await connect(withAuth: false)
                await startSync()
                return
            }
            try await auth.signIn(using: SystemWebAuthenticator())
            // the state stream flips authPhase and starts sync
        } catch AuthError.cancelled {
        } catch {
            lastError = "Sign-in failed: \(error)"
        }
    }

    /// Revoke the grant on the server and clear the Keychain. The cache
    /// stays (it is per server, and the next sign-in resumes from it).
    func signOut() async {
        await stopSync()
        await auth?.signOut()
    }

    private func authChanged(_ state: AuthSession.State) async {
        authPhase = state == .signedIn ? .signedIn : .signedOut
        if state == .signedIn {
            await sync?.start()
        } else {
            await sync?.stop()
        }
    }

    static func isLoopback(_ url: URL) -> Bool {
        ["127.0.0.1", "localhost", "::1"].contains(url.host() ?? "")
    }

    /// The server decides, not the URL: one with tokens in the Keychain is
    /// signed in; otherwise discovery says whether it needs sign-in (a
    /// SERVER-mode daemon can sit on localhost behind a tunnel, and a
    /// LOCAL-mode one can be reached by name). Unreachable and no tokens:
    /// a loopback daemon is assumed LOCAL, anything else asks to sign in.
    static func authSession(for url: URL) async throws -> AuthSession? {
        let auth = AuthSession(oauth: OAuthClient(baseURL: url), store: try KeychainTokenStore())
        if await auth.state == .signedIn { return auth }
        do {
            return try await auth.requiresAuth() ? auth : nil
        } catch {
            return isLoopback(url) ? nil : auth
        }
    }

    /// One cache file per server, so switching servers never mixes docs.
    private func connect(withAuth: Bool = true) async {
        observeTask?.cancel()
        authTask?.cancel()
        guard let url = URL(string: serverURL) else {
            lastError = "not a URL: \(serverURL)"
            return
        }
        do {
            let dir = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            let name = "cache-\(url.host() ?? "server")-\(url.port ?? 0).sqlite"
            let cache = try Cache(path: dir.appending(path: name).path(percentEncoded: false))
            let auth = withAuth ? try await Self.authSession(for: url) : nil
            let api = APIClient(config: ServerConfig(baseURL: url, tokenProvider: auth ?? NoAuth()))
            self.auth = auth
            authPhase = auth == nil ? .notRequired : (await auth?.state == .signedIn ? .signedIn : .signedOut)
            if let auth {
                authTask = Task { [weak self] in
                    for await state in await auth.states() {
                        await self?.authChanged(state)
                    }
                }
            }
            self.cache = cache
            self.api = api
            sync = SyncEngine(api: api, cache: cache)
            dueAlerts.connect(cache: cache, api: api, sync: sync)
            observeTask = Task { [weak self] in
                do {
                    for try await docs in cache.observeTree() {
                        self?.docs = docs
                        self?.tree = DocTreeNode.build(docs)
                    }
                } catch {
                    self?.lastError = error.localizedDescription
                }
            }
        } catch {
            lastError = error.localizedDescription
        }
    }

    func doc(titled title: String) -> DocRecord? { Library.doc(titled: title, in: docs) }

    var todoDocID: DocID? { Library.todoDocID(in: docs) }
}
