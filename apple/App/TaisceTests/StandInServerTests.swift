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
    static let todoDay = #"{"carried":0,"date":"2026-10-01","doc_id":"t","epoch":3,"items":[],"prev_date":null,"today":"2026-10-01"}"#

    static func reset() {
        log.withLock { $0 = [] }
        held.withLock { $0 = [] }
        routes.withLock { $0 = [:] }
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
}
