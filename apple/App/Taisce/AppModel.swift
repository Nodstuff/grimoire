import Foundation
import TaisceKit
import Observation
import UIKit

/// App-wide state: the server connection, its sign-in, cache and sync
/// engine, the doc tree, pins, and the sync indicator. Everything below the
/// UI lives in TaisceKit; the cache and network calls run off the main actor.
@MainActor @Observable
final class AppModel {
    static let serverURLKey = "serverURL"
    static let defaultServerURL = "https://taisce.null.ie"

    enum AuthPhase: Equatable {
        /// discovery hasn't answered yet: show a neutral launch state
        case checking
        /// a LOCAL-mode daemon: no tokens
        case notRequired
        case signedOut
        case signedIn
    }

    private(set) var serverURL: String
    private(set) var api: APIClient?
    private(set) var cache: Cache?
    private(set) var sync: SyncEngine?
    private(set) var auth: AuthSession?
    private(set) var authPhase: AuthPhase = .checking
    private(set) var isSigningIn = false
    private(set) var docs: [DocInfo] = []
    private(set) var index = DocIndex([])
    private(set) var library: [LibraryNode] = []
    /// whether the cache has delivered the tree at least once
    private(set) var treeLoaded = false
    var lastError: String?
    let dueAlerts: NotificationCoordinator
    // push: APNs token registration (Push.swift)
    let push = PushRegistrar(store: UserDefaultsPushStore(), environment: PushConfig.environment, appVersion: PushConfig.appVersion())
    /// push: the registrar's queued work (fire-and-forget, never awaited by boot)
    @ObservationIgnored var pushTask: Task<Void, Never>?
    /// Server discovery (does it need sign-in?); tests swap in one with no network.
    @ObservationIgnored var discover: @MainActor (URL) async throws -> AuthSession? = { try await AppModel.authSession(for: $0) }

    // sync indicator
    private(set) var syncStatus: SyncStatus = .idle
    private(set) var lastSynced: Date?
    private(set) var cursor = 0
    private(set) var pendingWrites = 0
    private(set) var failedWrites = 0
    /// the engine's last connection/catch-up error (nil once healthy)
    private(set) var syncError: String?
    /// docs whose body fetch failed, with why (sync carries on without them)
    private(set) var failedDocs: [DocID: String] = [:]
    /// bumps whenever the To-do doc's cached blocks change (sync, first
    /// fetch, or our own writes landing): to-do lists reload on it
    private(set) var todoRevision = 0
    /// the first catch-up (or a cached body from an earlier run) has
    /// landed: before it, empty to-do lists mean "loading", not "nothing"
    private(set) var hasSynced = false
    /// overdue + due today, for the iPad sidebar's Today badge
    private(set) var dueCount = 0

    // workspaces (Workspaces.swift): Today, Library, Pinned, To-dos and
    // search show the current one; alerts span them all
    var workspaces: [Workspace] = []
    /// the server answered `GET /api/workspaces` (older daemons: no switcher, no filter)
    var workspacesSupported = false
    /// the last workspace chosen on this device, per server
    var storedWorkspace: WorkspaceScope?
    /// cached docs resolving to no workspace (Unsorted shows only while > 0)
    private(set) var unsortedDocCount = 0
    @ObservationIgnored var workspacesTask: Task<Void, Never>?

    // local state
    private(set) var pins: [DocID] = []
    private(set) var editMeta: [DocID: EditMeta] = [:]
    /// to-dos marked done or snoozed here, hidden until the server's list catches up
    private(set) var settledTodos: Set<String> = []
    /// a doc just created here: its screen opens in edit mode
    var pendingEditDoc: DocID?
    /// ADR 0004: docs just unshared from you, for every window to close
    private(set) var revocation: Revocation?
    /// the gentle note after an open doc was closed for that
    var accessNotice: String?
    /// a sign-in just happened (not a launch with stored tokens): the
    /// cache's owner check treats unclaimed data as someone else's
    @ObservationIgnored private var freshSignIn = false
    /// the cache's owner was checked for this connection
    @ObservationIgnored private var ownerChecked = false

    struct Revocation: Equatable {
        var docs: Set<DocID>
        var serial: Int
    }

    static let revokedNotice = "This doc is no longer shared with you."

    private var observeTask: Task<Void, Never>?
    private var authTask: Task<Void, Never>?
    private var updatesTask: Task<Void, Never>?
    private var pollTask: Task<Void, Never>?
    private var replaying = false
    private var todoObserveTask: Task<Void, Never>?
    private var observedTodoDoc: DocID?
    /// rendered blocks by (block id, content hash), shared across doc views
    let renderCache = RenderCache()

    /// `dueAlerts`: tests pass one over a fake notification center.
    init(dueAlerts: NotificationCoordinator = NotificationCoordinator()) {
        self.dueAlerts = dueAlerts
        let stored = UserDefaults.standard.string(forKey: Self.serverURLKey).flatMap { ServerConfig.normalizedURL($0)?.absoluteString }
        serverURL = stored.flatMap { ServerURLPolicy.accepts($0) ? $0 : nil } ?? Self.defaultServerURL
    }

    var needsSignIn: Bool { authPhase == .signedOut }
    var isCheckingAuth: Bool { authPhase == .checking }
    var isOnline: Bool { syncStatus == .live }

    func boot() async {
        guard api == nil else { return }
        // never let the permission read hold up the launch state
        Task { await dueAlerts.refresh() }
        await connect()
        await startSync()
    }

    func setServerURL(_ s: String) async {
        // "taisce.null.ie" means https://taisce.null.ie
        guard let url = ServerConfig.normalizedURL(s) else {
            lastError = ServerURLPolicy.Rejection.notAURL.message
            return
        }
        let trimmed = url.absoluteString
        guard trimmed != serverURL else { return }
        if case .failure(let rejection) = ServerURLPolicy.check(trimmed) {
            lastError = rejection.message
            return
        }
        await stopSync()
        await pushSessionEnding() // push
        serverURL = trimmed
        UserDefaults.standard.set(trimmed, forKey: Self.serverURLKey)
        await connect()
        await startSync()
    }

    func startSync() async {
        guard authPhase == .signedIn || authPhase == .notRequired else { return }
        // ADR 0004: whose cache this is, before anything reads or writes it
        await checkCacheOwner()
        await sync?.start()
        startPolling()
        // never awaited: the notification center's XPC can stall, and the
        // launch (or a server switch) must not wait on it
        let dueAlerts = dueAlerts
        Task { await dueAlerts.reconcile() }
        refreshWorkspaces() // workspaces
        pushSessionStarted() // push: fire-and-forget
    }

    func stopSync() async {
        pollTask?.cancel()
        pollTask = nil
        await sync?.stop()
        syncStatus = .idle
    }

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
            // consumed by the owner check when the state stream says signed in
            freshSignIn = true
            ownerChecked = false
            try await auth.signIn(using: SystemWebAuthenticator())
            // the state stream flips authPhase and starts sync
        } catch AuthError.cancelled {
            freshSignIn = false
        } catch {
            freshSignIn = false
            lastError = "Sign-in failed: \(error)"
        }
    }

    /// Revoke the grant on the server and clear the Keychain, then forget
    /// this person's data on this device (ADR 0004: the next person to sign
    /// in starts from nothing): the cache (docs, bodies, search index,
    /// to-dos, workspaces, cursor, queued writes), pins, the remembered
    /// workspace, and the due alerts planned from their lists. The client
    /// id stays (it is the app's, not the person's).
    func signOut() async {
        await stopSync()
        await pushSessionEnding() // push: before the revoke, while the bearer works
        await auth?.signOut()
        await forgetUserData()
    }

    /// Wipe everything this device holds for the signed-in person on this
    /// server: the cache and the per-user settings, then re-plan alerts
    /// (from nothing, so every pending one goes).
    func forgetUserData() async {
        do {
            try await cache?.wipe()
        } catch {
            lastError = "Couldn't clear this device's copy: \(error.localizedDescription)"
        }
        await clearUserState()
        ownerChecked = false
    }

    /// The per-person state beside the cache: settings in UserDefaults,
    /// what the model holds in memory, and the alerts planned from it.
    private func clearUserState() async {
        UserSettings(server: serverURL).forget()
        pins = []
        storedWorkspace = nil
        workspaces = []
        workspacesSupported = false
        editMeta = [:]
        settledTodos = []
        pendingEditDoc = nil
        pendingWrites = 0
        failedWrites = 0
        failedDocs = [:]
        cursor = 0
        hasSynced = false
        await sync?.setAlwaysFetch([])
        await dueAlerts.reconcile()
    }

    /// The cache must be the signed-in person's (`CacheOwnership`): another
    /// person's (or unclaimed data under a fresh sign-in) is wiped first.
    /// Once per connection; LOCAL mode has one person and skips it.
    func checkCacheOwner() async {
        guard !ownerChecked, auth != nil, authPhase == .signedIn, let cache, let api else { return }
        ownerChecked = true
        let fresh = freshSignIn
        freshSignIn = false
        guard let decision = try? await CacheOwnership.reconcile(cache: cache, api: api, freshSignIn: fresh) else { return }
        // the cache is wiped; the settings that went with it go too
        if case .wipe = decision { await clearUserState() }
    }

    private func authChanged(_ state: AuthSession.State) async {
        authPhase = state == .signedIn ? .signedIn : .signedOut
        if state == .signedIn {
            await startSync()
        } else {
            // a sign-out we didn't ask for (the grant was revoked or ran
            // out) keeps the data: the same person signing back in keeps
            // their queued writes; anyone else gets a wiped cache
            // (`checkCacheOwner`)
            ownerChecked = false
            await stopSync()
            await pushSessionEnding() // push
        }
    }

    // push: a silent push can launch the app in the background, before boot
    func connectIfNeeded() async {
        guard api == nil else { return }
        await connect()
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
        // the universal-link sign-in where this build claims the host (Release: taisce.null.ie)
        let oauth = OAuthClient(baseURL: url, appLinkHosts: AppLinks.hosts())
        let auth = AuthSession(oauth: oauth, store: try KeychainTokenStore(), shield: Self.backgroundTask)
        if await auth.state == .signedIn { return auth }
        do {
            return try await auth.requiresAuth() ? auth : nil
        } catch {
            return isLoopback(url) ? nil : auth
        }
    }

    /// Finish a token refresh even if the app is backgrounded mid-request:
    /// a rotation the server did but we never saved costs the whole grant.
    static let backgroundTask: AuthSession.Shield = { name in
        let id = await MainActor.run { UIApplication.shared.beginBackgroundTask(withName: name) }
        return { await MainActor.run { UIApplication.shared.endBackgroundTask(id) } }
    }

    /// One cache file per server, so switching servers never mixes docs.
    private func connect(withAuth: Bool = true) async {
        observeTask?.cancel()
        authTask?.cancel()
        updatesTask?.cancel()
        todoObserveTask?.cancel()
        observedTodoDoc = nil
        hasSynced = false
        ownerChecked = false
        guard let url = URL(string: serverURL) else {
            lastError = "not a URL: \(serverURL)"
            authPhase = .notRequired
            return
        }
        do {
            let dir = try FileManager.default.url(for: .applicationSupportDirectory, in: .userDomainMask, appropriateFor: nil, create: true)
            let name = "cache-\(url.host() ?? "server")-\(url.port ?? 0).sqlite"
            let cache = try Cache(path: dir.appending(path: name).path(percentEncoded: false))
            authPhase = .checking
            let auth = withAuth ? try await discover(url) : nil
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
            let sync = SyncEngine(api: api, cache: cache)
            self.sync = sync
            dueAlerts.connect(cache: cache, api: api, sync: sync)
            pins = UserDefaults.standard.stringArray(forKey: pinsKey) ?? []
            await loadCachedWorkspaces(cache) // workspaces
            editMeta = [:]
            treeLoaded = false
            await sync.setAlwaysFetch(Set(pins))
            observeTask = Task { [weak self] in
                do {
                    for try await records in cache.observeTree() {
                        await self?.treeChanged(records)
                    }
                } catch {
                    self?.lastError = error.localizedDescription
                }
            }
            let updates = await sync.updates()
            updatesTask = Task { [weak self] in
                for await u in updates { self?.synced(u) }
            }
        } catch {
            lastError = error.localizedDescription
            // the error shows in Settings; never leave the UI on "checking"
            if authPhase == .checking { authPhase = .notRequired }
        }
    }

    private func treeChanged(_ records: [DocRecord]) async {
        let docs = records.map(DocInfo.init)
        let unsorted = docs.count { $0.workspaceID == nil }
        let scope = WorkspacePicker(workspaces: workspaces, unsortedCount: unsorted, stored: storedWorkspace, enabled: hasWorkspaces).current
        // the outline is O(n log n) over the whole tree: build it off the main actor
        let (index, library) = await Task.detached { (DocIndex(docs), LibraryNode.build(WorkspaceFilter.docs(docs, in: scope))) }.value
        self.docs = docs
        self.index = index
        self.library = library
        unsortedDocCount = unsorted
        treeLoaded = true
        observeTodoDoc()
    }

    /// Follow the To-do doc's blocks, so Today and To-dos reload whenever
    /// its body is (re)stored, including the first fetch after bootstrap.
    private func observeTodoDoc() {
        guard let id = todoDocID, id != observedTodoDoc, let cache else { return }
        observedTodoDoc = id
        todoObserveTask?.cancel()
        todoObserveTask = Task { [weak self] in
            do {
                for try await blocks in cache.observeBlocks(of: id) {
                    if !blocks.isEmpty { self?.hasSynced = true }
                    self?.todoRevision += 1
                }
            } catch {
                self?.lastError = error.localizedDescription
            }
        }
    }

    private func synced(_ u: SyncUpdate) {
        lastSynced = .now
        if u.treeChanged || u.docIDs.contains(where: { $0 == todoDocID }) { todoRevision += 1 }
        // a label change journals tree rows: names and counts may have moved;
        // an access change (a share, an unshare, a role) too
        if u.treeChanged || u.accessChanged { refreshWorkspaces() }
        for id in u.docIDs { editMeta[id] = nil }
        if !u.revokedDocIDs.isEmpty { revoked(u.revokedDocIDs) }
    }

    /// ADR 0004: docs no longer shared with you. Sync has dropped them from
    /// the cache (bodies, search, to-dos, queued writes); pins and per-doc
    /// state go here; windows showing one close it (`Router.drop`).
    func revoked(_ ids: Set<DocID>) {
        let before = pins
        pins.removeAll { ids.contains($0) }
        if pins != before {
            UserDefaults.standard.set(pins, forKey: pinsKey)
            let keep = Set(pins)
            Task { await sync?.setAlwaysFetch(keep) }
        }
        for id in ids { editMeta[id] = nil }
        if let p = pendingEditDoc, ids.contains(p) { pendingEditDoc = nil }
        revocation = Revocation(docs: ids, serial: (revocation?.serial ?? 0) + 1)
    }

    /// The indicator reads the engine and outbox every couple of seconds
    /// (both are cheap actor/GRDB reads), and replays queued writes when
    /// the connection is back.
    private func startPolling() {
        pollTask?.cancel()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                await self?.pollOnce()
                try? await Task.sleep(for: .seconds(2))
            }
        }
    }

    private func pollOnce() async {
        guard let sync, let cache else { return }
        let status = await sync.status
        syncError = await sync.lastError
        failedDocs = await sync.failedDocs
        if status == .live, syncStatus != .live {
            lastSynced = .now
            hasSynced = true
        }
        syncStatus = status
        cursor = (try? await cache.lastSeq()) ?? cursor
        let pending = (try? await cache.pendingOutbox().count) ?? 0
        pendingWrites = pending
        if pending > 0, status == .live { await replayOutbox() }
    }

    /// Send queued writes in order. Single-flight; failures stay on the row.
    func replayOutbox() async {
        guard let api, let cache, !replaying else { return }
        replaying = true
        defer { replaying = false }
        let report = try? await OutboxReplayer(api: api, cache: cache).replay()
        pendingWrites = (try? await cache.pendingOutbox().count) ?? pendingWrites
        if pendingWrites == 0 { todoRevision += 1 }
        if let report, report.forbidden > 0 {
            // ADR 0004: the role changed under us; the switcher and the
            // editing affordances follow the server's answer
            lastError = OutboxReplayer.readOnlyMessage
            refreshWorkspaces()
        }
    }

    // MARK: pins (local for now)

    private var pinsKey: String { UserSettings(server: serverURL).pinsKey }

    func isPinned(_ id: DocID) -> Bool { pins.contains(id) }

    func togglePin(_ id: DocID) {
        if let i = pins.firstIndex(of: id) { pins.remove(at: i) } else { pins.append(id) }
        UserDefaults.standard.set(pins, forKey: pinsKey)
        let ids = Set(pins)
        Task { await sync?.setAlwaysFetch(ids) }
    }

    // MARK: docs

    /// The current workspace's To-do doc (legacy: the root one).
    var todoDocID: DocID? {
        WorkspaceFilter.todoDoc(in: docs, index: index, scope: currentWorkspace)
    }

    /// Switch workspace: remembered per server, and every screen re-reads.
    func selectWorkspace(_ scope: WorkspaceScope) {
        storedWorkspace = scope
        WorkspacePreference(key: workspaceKey).save(scope)
        workspaceDidChange()
    }

    /// The current workspace moved (a choice, or the stored one vanished).
    func workspaceDidChange() {
        library = LibraryNode.build(WorkspaceFilter.docs(docs, in: currentWorkspace))
        observeTodoDoc()
        todoRevision += 1
    }

    var workspaceKey: String { UserSettings(server: serverURL).workspaceKey }

    /// Who last edited the doc, from its ledger; cached until the doc changes.
    func loadEditMeta(_ id: DocID) async {
        guard editMeta[id] == nil, let api else { return }
        if let history = try? await api.docHistory(id), let meta = EditMeta(history: history) {
            editMeta[id] = meta
        }
    }

    /// The doc's page: cached blocks, parsed off the main actor; blocks
    /// whose content hasn't changed come from `renderCache`.
    func page(for id: DocID, records: [BlockRecord]) async -> DocPage {
        let title = index.byID[id]?.title ?? ""
        var blocks = records.map(\.block)
        // edits still in the outbox show as they will land
        if pendingWrites > 0, let cache, let editor = try? await cache.editor(for: id) {
            blocks = editor.ordered().map(\.block)
        }
        let cache = renderCache
        return await Task.detached { DocPage.build(title: title, blocks: blocks, render: cache.nodes(for:)) }.value
    }

    /// Tick or untick the `index`th checkbox in a block: a `replace` through
    /// the outbox, then an immediate replay.
    func setCheckbox(doc: DocID, block: BlockID, index: Int, checked: Bool) async throws {
        guard let cache else { return }
        guard var editor = try await cache.editor(for: doc), let current = editor.blocks[block],
              let text = Checkbox.toggled(current.content, index: index, checked: checked)
        else { throw TodoWriteError.notFound }
        try await cache.enqueue([.replaceText(block, text)], on: &editor)
        pendingWrites += 1
        await replayOutbox()
    }

    /// A new doc under `parent` (nil = top level), created once even if
    /// the answer is lost, and put in the tree at once.
    func createDoc(title: String, parent: DocID?) async throws -> DocID {
        guard let api, let cache else { throw TodoWriteError.notFound }
        // workspaces: a root doc lands in the current workspace; a child inherits
        let doc = try await NewDoc.create(
            api: api, title: title, parent: parent, known: Set(docs.map(\.id)),
            workspaceID: parent == nil ? currentWorkspace?.workspaceID : nil
        )
        let inherited = parent.flatMap { index.byID[$0]?.workspaceID }
        try await cache.applyDocState(doc.id, Change.DocState(
            title: doc.title, parentID: doc.parentID, sortKey: doc.sortKey, status: doc.status, currentEpoch: doc.currentEpoch,
            workspaceID: doc.workspaceID ?? inherited ?? (parent == nil ? currentWorkspace?.workspaceID : nil)
        ))
        pendingEditDoc = doc.id
        return doc.id
    }

    // MARK: to-dos

    /// Open to-dos: dated ones from the server's read-only due list (`api.todoDue`; overdue
    /// computed on the device), today's undated ones from the cached To-do doc. Offline,
    /// both come from the cache. Never the day read (a GET for today carries items forward).
    func loadTodos(now: Date = .now) async -> (board: TodoBoard, offline: Bool) {
        // this workspace's list only
        let scope = currentWorkspace
        var cached: [TodoRecord] = []
        if let id = todoDocID, let cache { cached = (try? await cache.todos(in: id)) ?? [] }
        let today = TodoClock(now: now).today
        let undated = cached.filter { $0.isOpen && $0.date == today && $0.due == nil }.map { TodoEntry($0, now: now) }
        var dated: [TodoEntry]
        var offline = false
        if let api, let list = try? await api.todoDue(workspace: scope) {
            dated = list.items.map(TodoEntry.init)
        } else {
            offline = true
            dated = cached.filter { $0.isOpen && $0.due != nil }.map { TodoEntry($0, now: now) }
        }
        let settled = settledTodos
        if pendingWrites == 0 { settledTodos = [] }
        let visible = dated.filter { !settled.contains($0.id) }
        let board = TodoBoard.build(dated: visible, undated: undated.filter { !settled.contains($0.id) }, now: now)
        dueCount = board.due.count
        return (board, offline)
    }

    // Every to-do write goes through TaisceKit's typed, queued calls (each
    // carries this device's TodoClock); none spells a route here.

    func markDone(_ e: TodoEntry) async {
        await queue(settling: e) { cache in
            try await cache.enqueueTodoToggle(date: e.date, itemID: e.itemID, done: true, clock: todoClock())
        }
    }

    /// The new deadline is a wall time on this device, stored as UTC.
    func snooze(_ e: TodoEntry, _ s: Snooze) async {
        guard let due = s.deadline(), let deadline = Deadline.local(due) else { return }
        await queue(settling: e) { cache in
            try await cache.enqueueDeadline(date: e.date, itemID: e.itemID, deadline: deadline, clock: todoClock())
        }
    }

    /// A new to-do on today; the server reads any `due …` phrase in it.
    func addTodo(_ text: String) async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        let clock = todoClock()
        await queue(settling: nil) { cache in
            try await cache.enqueueTodoAdd(date: clock.today, text: trimmed, clock: clock)
        }
    }

    /// A checkbox in the To-do doc itself: the item's typed toggle, not a
    /// block edit (the to-do routes keep the doc's canonical form).
    func setTodoDone(date: String, position: Int, done: Bool) async throws {
        guard let cache else { return }
        guard let todoDocID, let record = try await cache.todos(in: todoDocID).first(where: { $0.date == date && $0.position == position }) else {
            throw TodoWriteError.notFound
        }
        let itemID = TodoEntry(record).itemID
        await queue(settling: nil) { cache in
            try await cache.enqueueTodoToggle(date: date, itemID: itemID, done: done, clock: todoClock())
        }
    }

    /// The server's reading of a typed to-do, for the live hint.
    func parseHint(_ text: String) async -> TodoParseHint? {
        try? await api?.todoParse(text)
    }

    private func queue(settling e: TodoEntry?, _ write: (Cache) async throws -> Void) async {
        guard let cache else { return }
        if let e { settledTodos.insert(e.id) }
        do {
            try await write(cache)
            pendingWrites += 1
            todoRevision += 1
            await replayOutbox()
        } catch {
            if let e { settledTodos.remove(e.id) }
            lastError = error.localizedDescription
        }
    }

    // MARK: search

    /// Server search, falling back to the offline FTS index when the server
    /// can't be reached. Tags come from cached frontmatter (`fillTags` fetches
    /// the rest).
    /// The current workspace's docs unless `everywhere`.
    func search(_ query: String, everywhere: Bool = false) async -> SearchState {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return SearchState() }
        let scope = everywhere ? nil : currentWorkspace
        var hits: [(block: BlockID, doc: DocID, title: String, content: String)] = []
        var offline = false
        if let api, let found = try? await api.search(q, workspace: scope) {
            hits = found.map { ($0.block.id, $0.block.docID, $0.docTitle, $0.block.content) }
        } else if let cache, let found = try? await cache.searchBlocks(q) {
            offline = true
            let index = index
            hits = found
                .filter { WorkspaceFilter.keeps(index.byID[$0.docID], scope: scope) }
                .map { ($0.id, $0.docID, index.byID[$0.docID]?.title ?? "", $0.content) }
        }
        var tags: [DocID: [String]] = [:]
        for id in Set(hits.map(\.doc)) { tags[id] = await cachedTags(id) }
        let index = index
        return await Task.detached { SearchState.build(hits: hits, query: q, index: index, tags: tags, offline: offline) }.value
    }

    /// Frontmatter tags of a cached body; nil when the body was never fetched.
    func cachedTags(_ id: DocID) async -> [String]? {
        guard let cache, let rec = try? await cache.doc(id), rec.bodyEpoch != nil else { return nil }
        let blocks = (try? await cache.blocks(of: id)) ?? []
        return blocks.filter { $0.content.hasPrefix("---") }.flatMap { DocPage.frontmatterTags($0.content) }
    }

    /// Fetch up to eight result docs we hold no body for, so their tags
    /// (and offline search) fill in; returns the tags found.
    func fillTags(_ state: SearchState) async -> SearchState {
        guard let sync else { return state }
        var state = state
        var fetched = 0
        for i in state.results.indices where fetched < 8 {
            let id = state.results[i].docID
            guard await cachedTags(id) == nil else { continue }
            fetched += 1
            try? await sync.refresh(id)
            state.results[i].tags = await cachedTags(id) ?? []
        }
        return state
    }
}

enum TodoWriteError: Error, LocalizedError {
    case notFound

    var errorDescription: String? { "That item changed on the server; pull to refresh." }
}
