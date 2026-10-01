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

    private var observeTask: Task<Void, Never>?
    private var authTask: Task<Void, Never>?

    init() {
        serverURL = UserDefaults.standard.string(forKey: Self.serverURLKey) ?? Self.defaultServerURL
    }

    var needsSignIn: Bool { authPhase == .signedOut }

    func boot() async {
        guard api == nil else { return }
        await connect()
        await startSync()
    }

    func setServerURL(_ s: String) async {
        let trimmed = s.trimmingCharacters(in: .whitespacesAndNewlines)
        guard trimmed != serverURL, URL(string: trimmed) != nil else { return }
        await stopSync()
        serverURL = trimmed
        UserDefaults.standard.set(trimmed, forKey: Self.serverURLKey)
        await connect()
        await startSync()
    }

    func startSync() async {
        guard authPhase != .signedOut else { return }
        await sync?.start()
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

    /// Loopback daemons run in LOCAL mode (loopback is trusted); anything
    /// else gets an `AuthSession` with tokens in the Keychain.
    static func isLoopback(_ url: URL) -> Bool {
        ["127.0.0.1", "localhost", "::1"].contains(url.host() ?? "")
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
            let auth = withAuth && !Self.isLoopback(url)
                ? AuthSession(oauth: OAuthClient(baseURL: url), store: KeychainTokenStore())
                : nil
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

    func doc(titled title: String) -> DocRecord? {
        // wikilinks name docs by title, optionally with a parent path
        let leaf = title.split(separator: "/").last.map(String.init) ?? title
        return docs.first { $0.title == title } ?? docs.first { $0.title == leaf }
    }

    var todoDocID: DocID? {
        docs.first { $0.parentID == nil && $0.title == TodoParser.todoDocTitle }?.id
    }
}

/// The flat doc list as a tree for `OutlineGroup` (nil children = leaf).
struct DocTreeNode: Identifiable, Hashable {
    var doc: DocRecord
    var children: [DocTreeNode]?
    var id: DocID { doc.id }

    static func build(_ docs: [DocRecord]) -> [DocTreeNode] {
        // a doc whose parent we don't have (trashed, not shared to us) shows at the root
        let ids = Set(docs.map(\.id))
        let byParent = Dictionary(grouping: docs) { $0.parentID.flatMap { ids.contains($0) ? $0 : nil } }
        func nodes(_ parent: DocID?) -> [DocTreeNode] {
            (byParent[parent] ?? [])
                .sorted { ($0.sortKey ?? "", $0.title) < ($1.sortKey ?? "", $1.title) }
                .map { d in
                    let kids = nodes(d.id)
                    return DocTreeNode(doc: d, children: kids.isEmpty ? nil : kids)
                }
        }
        return nodes(nil)
    }
}
