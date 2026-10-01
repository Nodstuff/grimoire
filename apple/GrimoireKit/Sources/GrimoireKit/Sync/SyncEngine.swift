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

    private var runner: Task<Void, Never>?
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

    public func start() {
        guard runner == nil else { return }
        runner = Task { await self.run() }
    }

    public func stop() {
        runner?.cancel()
        runner = nil
        status = .idle
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
            } catch {
                // fall through to the backoff; a failed catch-up is retried whole
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
    /// A fresh cache (cursor 0) first loads the whole tree.
    public func catchUp() async throws {
        var since = try await cache.lastSeq()
        if since == 0 {
            try await cache.replaceTree(api.tree())
            publish(SyncUpdate(docIDs: [], treeChanged: true))
        }
        while true {
            try Task.checkCancellation()
            let page = try await api.changes(since: since, limit: pageSize)
            publish(try await apply(page.changes))
            // never move the cursor backwards, even on a confused server
            since = max(since, page.seq)
            try await cache.setLastSeq(since)
            if !page.more || page.changes.isEmpty { break }
        }
    }

    /// Follow the SSE stream until it ends or drops.
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
                guard e.event == "change" || e.event == "message",
                      let data = e.data.data(using: .utf8),
                      let change = try? JSONDecoder().decode(Change.self, from: data)
                else { continue }
                publish(try await apply([change]))
                let seq = max(change.seq, e.id.flatMap(Int.init) ?? 0)
                if seq > (try await cache.lastSeq()) { try await cache.setLastSeq(seq) }
            }
        }
    }

    // MARK: applying changes

    /// Apply a batch: the LAST change per doc wins (a doc edited then
    /// deleted in one page is just deleted). Tree-shaped changes refetch the
    /// tree once; doc changes refetch bodies we hold, else mark them stale.
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
        if changes.contains(where: { $0.kind == .tree || $0.kind == .restored }) {
            try await cache.replaceTree(api.tree())
            update.treeChanged = true
        }
        for id in order {
            guard let c = last[id], c.kind != .deleted else { continue }
            let cached = try await cache.doc(id)
            let wanted = alwaysFetch.contains(id)
                || (cached.map { $0.parentID == nil && $0.title == TodoParser.todoDocTitle } ?? false)
            let held = cached?.bodyEpoch != nil
            if held || wanted {
                if let have = cached?.bodyEpoch, let epoch = c.epoch, have >= epoch { continue }
                try await cache.storeDoc(api.doc(id))
                update.docIDs.insert(id)
            } else if let epoch = c.epoch {
                try await cache.noteEpoch(id, epoch: epoch)
            }
        }
        return update
    }

    /// Fetch one doc now (opening a doc whose body is missing or stale).
    public func refresh(_ id: DocID) async throws {
        try await cache.storeDoc(api.doc(id))
        publish(SyncUpdate(docIDs: [id], treeChanged: false))
    }
}
