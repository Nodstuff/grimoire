import Foundation

/// What a sync step changed, for the UI to re-read.
public struct SyncUpdate: Sendable, Hashable {
    /// docs whose cached body was refreshed or removed
    public var docIDs: Set<DocID>
    /// the doc tree (titles, parents, order, membership) changed
    public var treeChanged: Bool

    public var isEmpty: Bool { docIDs.isEmpty && !treeChanged }
}

public enum SyncStatus: Sendable, Hashable {
    case idle
    case catchingUp
    case live
    case waiting(retryIn: Duration)
}

/// Keeps the cache in step with the server through the global change cursor:
/// page `/api/changes` from `last_seq`, then follow `/api/changes/stream`;
/// on a drop, back off and resume from `last_seq` via `Last-Event-ID`.
///
/// Foreground-only: the app calls `start()` on scene activation and `stop()`
/// on backgrounding. Background refresh / APNs come later.
public actor SyncEngine {
    let api: APIClient
    let cache: Cache
    let backoff: Backoff
    let sleep: @Sendable (Duration) async throws -> Void
    let pageSize: Int

    public private(set) var status: SyncStatus = .idle
    /// Docs whose bodies are fetched on change even if never opened
    /// (the To-do doc is always included; pinned docs later).
    public var alwaysFetch: Set<DocID> = []

    /// Bodies that failed to fetch (doc id → error), kept stale and retried
    /// on the doc's next change or when it is opened. The UI can show these.
    public private(set) var failedDocs: [DocID: String] = [:]
    public private(set) var lastError: String?

    private var runner: Task<Void, Never>?
    /// The loop `stop()` cancelled, until it has unwound: `start()` waits for
    /// it so two loops never write the cursor at once.
    private var stopping: Task<Void, Never>?
    private var subscribers: [UUID: AsyncStream<SyncUpdate>.Continuation] = [:]
    private var serverRetry: Duration?
    /// set by `follow()` when the current connection delivered anything
    private var connectionHealthy = false

    public init(
        api: APIClient,
        cache: Cache,
        backoff: Backoff = Backoff(),
        pageSize: Int = 500,
        sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) {
        self.api = api
        self.cache = cache
        self.backoff = backoff
        self.pageSize = pageSize
        self.sleep = sleep
    }

    // MARK: lifecycle

    public func start() async {
        await stopping?.value
        guard runner == nil else { return }
        runner = Task { await self.run() }
    }

    /// Cancel the loop and wait for it to finish.
    public func stop() async {
        guard let old = runner else { return }
        runner = nil
        old.cancel()
        stopping = old
        await old.value
        if stopping == old { stopping = nil }
        if runner == nil { status = .idle }
    }

    public func setAlwaysFetch(_ ids: Set<DocID>) {
        alwaysFetch = ids
    }

    /// A fresh stream of updates; each subscriber gets every update.
    public func updates() -> AsyncStream<SyncUpdate> {
        let (stream, continuation) = AsyncStream<SyncUpdate>.makeStream(bufferingPolicy: .bufferingNewest(64))
        let id = UUID()
        subscribers[id] = continuation
        continuation.onTermination = { _ in Task { await self.unsubscribe(id) } }
        return stream
    }

    private func unsubscribe(_ id: UUID) {
        subscribers[id] = nil
    }

    private func publish(_ u: SyncUpdate) {
        guard !u.isEmpty else { return }
        for c in subscribers.values { c.yield(u) }
    }

    // MARK: the loop

    func run() async {
        var attempt = 0
        var caughtUp = false
        while !Task.isCancelled {
            do {
                if !caughtUp {
                    status = .catchingUp
                    try await catchUp()
                    caughtUp = true
                }
                status = .live
                try await follow()
            } catch is CancellationError {
                break
            } catch SyncError.undecodableEvent {
                // re-read that stretch through /api/changes before following
                // again (after the usual backoff, so a server that keeps
                // sending it can't spin us)
                caughtUp = false
                lastError = "an undecodable change event; catching up"
            } catch {
                // fall through to the backoff; a failed catch-up is retried whole
                lastError = String(describing: error)
            }
            if Task.isCancelled { break }
            // a connection that delivered events or heartbeats was healthy:
            // its drop starts the backoff over
            if connectionHealthy { attempt = 0 }
            let delay = serverRetry.map { max($0, backoff.delay(attempt: attempt)) } ?? backoff.delay(attempt: attempt)
            status = .waiting(retryIn: delay)
            attempt += 1
            do { try await sleep(delay) } catch { break }
        }
    }

    /// Page the change log from the stored cursor until `more` is false.
    /// A fresh cache (cursor 0) first loads the whole tree and starts from
    /// the `Taisce-Seq` it came with, skipping the backfill. A head below
    /// our cursor means the server's database was reset or restored: start
    /// over from a fresh tree. Then fetch the To-do doc if it is stale, so
    /// Today has items on first run.
    public func catchUp() async throws {
        var since = try await cache.lastSeq()
        if since == 0 { since = try await bootstrap() }
        while true {
            try Task.checkCancellation()
            let page = try await api.changes(since: since, limit: pageSize)
            if page.seq < since {
                since = try await bootstrap(reset: true)
                continue
            }
            publish(try await apply(page.changes))
            // `seq` is the journal head, not this page's last row: with
            // `more` the next page starts after the last row we saw
            if let last = page.changes.last?.seq, last > since {
                since = last
                try await cache.setLastSeq(since)
            }
            if !page.more || page.changes.isEmpty { break }
        }
        try await fetchTodoIfStale()
        lastError = nil
    }

    /// Load the whole tree and start the cursor at its head. Returns the cursor.
    func bootstrap(reset: Bool = false) async throws -> Int {
        let (docs, head) = try await api.treeWithSeq()
        try await cache.replaceTree(docs)
        if reset {
            // cached bodies may describe the old database's epochs
            try await cache.markAllBodiesStale()
            try await cache.resetLastSeq(head ?? 0)
        } else if let head {
            try await cache.setLastSeq(head)
        }
        publish(SyncUpdate(docIDs: [], treeChanged: true))
        return head ?? 0
    }

    func fetchTodoIfStale() async throws {
        guard let todo = try await cache.docs().first(where: { $0.parentID == nil && $0.title == TodoParser.todoDocTitle }),
              todo.isBodyStale
        else { return }
        var update = SyncUpdate(docIDs: [], treeChanged: false)
        try await fetchBody(todo.id, target: todo.currentEpoch, into: &update)
        publish(update)
    }

    /// Follow the SSE stream until it ends or drops. An event we can't read
    /// leaves the cursor where it was and hands over to a catch-up.
    func follow() async throws {
        let from = try await cache.lastSeq()
        connectionHealthy = false
        for try await out in api.changeStream(lastEventID: from) {
            connectionHealthy = true
            switch out {
            case .retry(let ms):
                serverRetry = .milliseconds(ms)
            case .comment:
                break
            case .event(let e):
                guard e.event == "change" || e.event == "message" else { continue }
                guard let change = try? JSONDecoder().decode(Change.self, from: Data(e.data.utf8)) else {
                    throw SyncError.undecodableEvent(e.id)
                }
                publish(try await apply([change]))
                if change.seq > (try await cache.lastSeq()) { try await cache.setLastSeq(change.seq) }
            }
        }
    }

    // MARK: applying changes

    /// Apply a batch: the LAST change per doc wins (a doc edited then
    /// deleted in one page is just deleted). Tree-shaped changes apply the
    /// row's `doc` state, refetching the whole tree only when a row lacks it
    /// (older daemon); doc changes refetch bodies we hold, else mark them stale.
    func apply(_ changes: [Change]) async throws -> SyncUpdate {
        guard !changes.isEmpty else { return SyncUpdate(docIDs: [], treeChanged: false) }
        var last: [DocID: Change] = [:]
        var order: [DocID] = []
        for c in changes.sorted(by: { $0.seq < $1.seq }) {
            if last[c.docID] == nil { order.append(c.docID) }
            last[c.docID] = c
        }
        var update = SyncUpdate(docIDs: [], treeChanged: false)
        for id in order where last[id]?.kind == .deleted {
            try await cache.deleteDoc(id)
            update.docIDs.insert(id)
            update.treeChanged = true
        }
        // every row carries the doc's serve-time state: apply it first, so
        // titles and epochs update before any body refetch
        let live = order.compactMap { last[$0] }.filter { $0.kind != .deleted }
        if live.contains(where: { ($0.kind == .tree || $0.kind == .restored) && $0.doc == nil }) {
            try await cache.replaceTree(api.tree())
            update.treeChanged = true
        }
        for c in live {
            guard let s = c.doc else { continue }
            let before = try await cache.doc(c.docID)
            try await cache.applyDocState(c.docID, s)
            if c.kind == .tree || c.kind == .restored || before?.title != s.title || before?.parentID != s.parentID {
                update.treeChanged = true
            }
        }
        for id in order {
            guard let c = last[id], c.kind != .deleted else { continue }
            // the serve-time epoch is newer than or equal to the row's
            let target = c.doc?.currentEpoch ?? c.epoch
            let cached = try await cache.doc(id)
            let wanted = alwaysFetch.contains(id)
                || (cached.map { $0.parentID == nil && $0.title == TodoParser.todoDocTitle } ?? false)
            let held = cached?.bodyEpoch != nil
            if held || wanted {
                if let have = cached?.bodyEpoch, let target, have >= target { continue }
                try await fetchBody(id, target: target, into: &update)
            } else if let epoch = target {
                try await cache.noteEpoch(id, epoch: epoch)
            }
        }
        return update
    }

    /// One body fetch inside a batch. A doc the server no longer has is
    /// dropped; any other failure leaves it stale (so opening it retries)
    /// and is recorded in `failedDocs` instead of stalling the whole sync.
    /// Auth failures and cancellation still end the batch.
    func fetchBody(_ id: DocID, target: Int?, into update: inout SyncUpdate) async throws {
        do {
            try await cache.storeDoc(api.doc(id))
            failedDocs[id] = nil
            update.docIDs.insert(id)
        } catch APIError.notFound {
            try await cache.deleteDoc(id)
            failedDocs[id] = nil
            update.docIDs.insert(id)
            update.treeChanged = true
        } catch APIError.unauthorized {
            throw APIError.unauthorized
        } catch {
            if Task.isCancelled { throw CancellationError() }
            if let target { try await cache.noteEpoch(id, epoch: target) }
            failedDocs[id] = String(describing: error)
        }
    }

    /// Fetch one doc now (opening a doc whose body is missing or stale). A
    /// doc the server no longer has is dropped from the cache, then rethrown.
    public func refresh(_ id: DocID) async throws {
        do {
            try await cache.storeDoc(api.doc(id))
            failedDocs[id] = nil
            publish(SyncUpdate(docIDs: [id], treeChanged: false))
        } catch let e as APIError {
            if case .notFound = e {
                try await cache.deleteDoc(id)
                publish(SyncUpdate(docIDs: [id], treeChanged: true))
            }
            throw e
        }
    }
}

enum SyncError: Error {
    /// An SSE `change` whose data doesn't decode (carries the event id).
    case undecodableEvent(String?)
}
