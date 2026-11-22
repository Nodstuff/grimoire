import Foundation
import Synchronization
import Testing
import TaisceKit
@testable import Taisce

/// A stand-in server on one loopback port (other suites' requests pass
/// through untouched): records `/api/todo*` requests, answers the paths in
/// `routes` (to-do writes with a day), holds the answers to paths starting
/// with one of `held` until it is emptied, and answers 404 to anything else.
final class HeldServer: URLProtocol {
    static let port = 47
    static let log = Mutex<[URLRequest]>([])
    static let held = Mutex<[String]>([])
    static let routes = Mutex<[String: String]>([:])
    /// every path asked, in order
    static let hits = Mutex<[String]>([])
    static let todoDay = #"{"carried":0,"date":"2026-10-01","doc_id":"t","epoch":3,"items":[],"prev_date":null,"today":"2026-10-01"}"#

    static func reset() {
        log.withLock { $0 = [] }
        held.withLock { $0 = [] }
        routes.withLock { $0 = [:] }
        hits.withLock { $0 = [] }
    }

    override class func canInit(with request: URLRequest) -> Bool {
        request.url?.host() == "127.0.0.1" && request.url?.port == port
    }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        var req = request
        if req.httpBody == nil, let stream = req.httpBodyStream {
            stream.open()
            var data = Data()
            var buf = [UInt8](repeating: 0, count: 4096)
            while stream.hasBytesAvailable {
                let n = stream.read(&buf, maxLength: buf.count)
                if n <= 0 { break }
                data.append(buf, count: n)
            }
            stream.close()
            req.httpBody = data
        }
        let path = req.url?.path() ?? ""
        Self.hits.withLock { $0.append(path) }
        if path.hasPrefix("/api/todo") { Self.log.withLock { $0.append(req) } }
        answer(path: path, deadline: Date.now.addingTimeInterval(10))
    }

    private func answer(path: String, deadline: Date) {
        if Self.held.withLock({ $0.contains { path.hasPrefix($0) } }), Date.now < deadline {
            DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(10)) { self.answer(path: path, deadline: deadline) }
            return
        }
        let routed: String? = path.hasPrefix("/api/todo") && request.httpMethod == "POST" ? Self.todoDay : Self.routes.withLock { $0[path] }
        let status = routed == nil ? 404 : 200
        let body = routed ?? #"{"error":"not here"}"#
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(body.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}

    static var requestIDs: [String] {
        log.withLock { $0 }.compactMap { r in
            (r.httpBody.flatMap { try? JSONSerialization.jsonObject(with: $0) } as? [String: Any])?["request_id"] as? String
        }
    }

    /// A booted LOCAL-mode model on this server, sync stopped, its cache empty.
    @MainActor static func model(dueAlerts: NotificationCoordinator = NotificationCoordinator(center: FakeDueAlertCenter())) async throws -> AppModel {
        UserDefaults.standard.set("http://127.0.0.1:\(port)", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel(dueAlerts: dueAlerts)
        m.discover = { _ in nil }
        await m.boot()
        await m.stopSync()
        try await #require(m.cache).wipe()
        return m
    }
}

/// The model against `HeldServer` (one suite: the server's state is shared).
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct StandInServerTests {
    /// Review 2026-11-22 #2: every replay goes through AppModel's single-flight
    /// `replayOutbox`, so a due alert's action never sends a write twice.
    @Test func aDueAlertActionDuringAReplaySendsEachWriteOnce() async throws {
        URLProtocol.registerClass(HeldServer.self)
        defer { URLProtocol.unregisterClass(HeldServer.self) }
        HeldServer.reset()
        defer { HeldServer.reset() }
        let center = FakeDueAlertCenter()
        let m = try await HeldServer.model(dueAlerts: NotificationCoordinator(center: center))
        let cache = try #require(m.cache)
        #expect(m.mayReplay)
        HeldServer.held.withLock { $0 = ["/api/todo"] }

        // a tick queued in the app, its replay waiting on the server
        let first = try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: true,
                                                      clock: TodoClock(today: "2026-10-01", utcOffset: "+00:00"))
        let replay = Task { await m.replayOutbox() }
        for _ in 0..<500 where HeldServer.log.withLock({ $0.isEmpty }) { try await Task.sleep(for: .milliseconds(10)) }
        #expect(HeldServer.requestIDs == [first.idempotencyKey], "the app's replay has it in flight")

        // Done on a due alert meanwhile: it queues and asks for a replay
        await center.onAction?(.done, "2026-10-01", "0-b")
        #expect(HeldServer.requestIDs == [first.idempotencyKey], "nothing sent beside the running replay")

        HeldServer.held.withLock { $0 = [] }
        await replay.value
        let ids = HeldServer.requestIDs
        let queued = try await cache.pendingOutbox()
        #expect(queued.isEmpty, "the action's write went out in the same replay, not at the next poll")
        #expect(ids.count == 2 && Set(ids).count == 2 && ids.first == first.idempotencyKey, "each write once: \(ids)")
        try await cache.wipe()
    }

    /// Review of 28dc9cf: a row a cancelled replay left in flight (the poll
    /// task cancelled on backgrounding; the cache is not reopened on return)
    /// goes out at the next replay, before the writes queued after it.
    @Test func aRowLeftInFlightByACancelledReplayGoesFirstNextReplay() async throws {
        URLProtocol.registerClass(HeldServer.self)
        defer { URLProtocol.unregisterClass(HeldServer.self) }
        HeldServer.reset()
        defer { HeldServer.reset() }
        let m = try await HeldServer.model()
        let cache = try #require(m.cache)
        let clock = TodoClock(today: "2026-10-01", utcOffset: "+00:00")
        let tick = try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: true, clock: clock)
        // what a cancelled replay whose settle never landed leaves behind
        #expect(try await cache.claimOutbox(try #require(tick.id))?.state == .inflight)
        let untick = try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: false, clock: clock)
        await m.replayOutbox()
        #expect(HeldServer.requestIDs == [tick.idempotencyKey, untick.idempotencyKey], "the tick first, then the untick")
        #expect(try await cache.pendingOutbox().isEmpty)
        try await cache.wipe()
    }

    /// Review 2026-11-22 #4: Today's review queue and the person's name
    /// answer after a sign-out are the previous account's: dropped.
    @Test func homeAnswersAfterASignOutAreDropped() async throws {
        URLProtocol.registerClass(HeldServer.self)
        defer { URLProtocol.unregisterClass(HeldServer.self) }
        HeldServer.reset()
        defer { HeldServer.reset() }
        let m = try await HeldServer.model()
        HeldServer.routes.withLock { $0 = ["/api/queue": "[]", "/api/profile": #"{"name":"Aoife","principal_id":"aoife","confirmed":true}"#] }
        HeldServer.held.withLock { $0 = ["/api/queue", "/api/profile"] }
        let review = Task { await m.home.loadReview() }
        let profile = Task { await m.home.loadProfile() }
        try await Task.sleep(for: .milliseconds(100))
        m.home.reset() // what sign-out and a server switch do
        HeldServer.held.withLock { $0 = [] }
        await review.value
        await profile.value
        #expect(m.home.review == nil && m.home.reviewError == nil, "the last account's queue")
        #expect(m.home.profileName == nil, "the last account's name")
        // asked again, for the new account, they land
        await m.home.loadReview()
        await m.home.loadProfile()
        #expect(m.home.review == [] && m.home.profileName == "Aoife")
    }

    /// Review 2026-11-22 #7: a workspace switch while the tree is being
    /// built (off the main actor) shows the new workspace's docs, not the
    /// old one's when the build lands.
    @Test func aWorkspaceSwitchDuringATreeBuildShowsTheNewWorkspace() async throws {
        URLProtocol.registerClass(HeldServer.self)
        defer { URLProtocol.unregisterClass(HeldServer.self) }
        HeldServer.reset()
        defer { HeldServer.reset() }
        let m = try await HeldServer.model()
        let cache = try #require(m.cache)
        try await cache.replaceWorkspaces([Workspace(id: "w1", name: "Work"), Workspace(id: "w2", name: "Home")])
        await m.loadCachedWorkspaces(cache)
        m.selectWorkspace(.id("w1"))
        defer { UserDefaults.standard.removeObject(forKey: m.workspaceKey) }
        var switched = false
        m.afterTreeBuilt = {
            guard !switched else { return }
            switched = true
            m.selectWorkspace(.id("w2"))
        }
        try await cache.replaceTree([
            DocSummary(id: "work-doc", parentID: nil, title: "Work doc", currentEpoch: 1, workspaceID: "w1"),
            DocSummary(id: "home-doc", parentID: nil, title: "Home doc", currentEpoch: 1, workspaceID: "w2"),
        ])
        for _ in 0..<300 where !(switched && m.docs.count == 2) { try await Task.sleep(for: .milliseconds(10)) }
        #expect(switched)
        #expect(m.currentWorkspace == .id("w2"))
        #expect(m.library.map(\.doc.title) == ["Home doc"], "the old workspace's tree: \(m.library.map(\.doc.title))")
        m.afterTreeBuilt = nil
        try await cache.wipe()
    }

    /// Review 2026-11-22 #8: Library rows' edited times go out once per doc,
    /// four at a time, and land together.
    @Test func editedTimesAreFetchedOncePerDocFourAtATime() async throws {
        URLProtocol.registerClass(HeldServer.self)
        defer { URLProtocol.unregisterClass(HeldServer.self) }
        HeldServer.reset()
        defer { HeldServer.reset() }
        let m = try await HeldServer.model()
        let ids = (0..<10).map { "lib\($0)" }
        let row = #"[{"op":{"id":"0199a0b0-c0d0-7abc-8def-0123456789ab","doc_id":"x","epoch_applied":4},"principal_name":"Ann","principal_kind":"human"}]"#
        HeldServer.routes.withLock { r in for id in ids { r["/api/doc/\(id)/history"] = row } }
        HeldServer.held.withLock { $0 = ["/api/doc/"] }
        func asked() -> [String] { HeldServer.hits.withLock { $0 }.filter { $0.hasSuffix("/history") } }
        // every row comes on screen, some twice (a scroll back)
        for id in ids + ids.prefix(3) { m.wantEditMeta(id) }
        for _ in 0..<200 where asked().count < 4 { try await Task.sleep(for: .milliseconds(10)) }
        try await Task.sleep(for: .milliseconds(150))
        #expect(asked().count == 4, "four at a time: \(asked().count)")
        // a doc being fetched is not asked again, from a row or the doc's screen
        m.wantEditMeta("lib0")
        #expect(asked().contains("/api/doc/lib0/history"), "the first four go first")
        let screen = Task { await m.loadEditMeta("lib0") }
        let row0 = m.editedTimes.entry("lib0")
        HeldServer.held.withLock { $0 = [] }
        await screen.value
        for _ in 0..<300 where m.editMeta.count < 10 { try await Task.sleep(for: .milliseconds(10)) }
        #expect(m.editMeta.count == 10)
        #expect(asked().sorted() == ids.map { "/api/doc/\($0)/history" }.sorted(), "one request per doc")
        #expect(row0.date == Date(timeIntervalSince1970: Double(0x0199a0b0c0d0) / 1000), "the row's time arrived")
        // known: a row coming back asks nothing
        m.wantEditMeta("lib1")
        try await Task.sleep(for: .milliseconds(250))
        #expect(asked().count == 10)
    }
}
