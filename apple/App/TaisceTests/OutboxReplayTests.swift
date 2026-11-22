import Foundation
import Synchronization
import Testing
import TaisceKit
@testable import Taisce

/// A stand-in server on one loopback port (other suites' requests pass
/// through untouched): records every request, holds `/api/todo*` answers
/// while `hold` is set, answers 404 to anything else.
final class HeldTodoServer: URLProtocol {
    static let port = 47
    static let log = Mutex<[URLRequest]>([])
    static let hold = Mutex(false)

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
        let todo = req.url?.path().hasPrefix("/api/todo") == true
        if todo { Self.log.withLock { $0.append(req) } }
        answer(todo: todo, deadline: Date.now.addingTimeInterval(10))
    }

    private func answer(todo: Bool, deadline: Date) {
        if todo, Self.hold.withLock({ $0 }), Date.now < deadline {
            DispatchQueue.global().asyncAfter(deadline: .now() + .milliseconds(10)) { self.answer(todo: todo, deadline: deadline) }
            return
        }
        let status = todo ? 200 : 404
        let body = todo ? #"{"carried":0,"date":"2026-10-01","doc_id":"t","epoch":3,"items":[],"prev_date":null,"today":"2026-10-01"}"# : #"{"error":"not here"}"#
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
}

/// Review 2026-11-22 #2: every replay goes through AppModel's single-flight
/// `replayOutbox`, so a due alert's action never sends a write twice.
@MainActor @Suite(.serialized) struct OutboxReplayTests {
    @Test func aDueAlertActionDuringAReplaySendsEachWriteOnce() async throws {
        URLProtocol.registerClass(HeldTodoServer.self)
        defer { URLProtocol.unregisterClass(HeldTodoServer.self) }
        HeldTodoServer.log.withLock { $0 = [] }
        HeldTodoServer.hold.withLock { $0 = true }
        defer { HeldTodoServer.hold.withLock { $0 = false } }
        let server = "http://127.0.0.1:\(HeldTodoServer.port)"
        UserDefaults.standard.set(server, forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }

        let center = FakeDueAlertCenter()
        let m = AppModel(dueAlerts: NotificationCoordinator(center: center))
        m.discover = { _ in nil }
        await m.boot()
        await m.stopSync()
        let cache = try #require(m.cache)
        try await cache.wipe()
        #expect(m.mayReplay)

        // a tick queued in the app, its replay waiting on the server
        let first = try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: true,
                                                      clock: TodoClock(today: "2026-10-01", utcOffset: "+00:00"))
        let replay = Task { await m.replayOutbox() }
        for _ in 0..<500 where HeldTodoServer.log.withLock({ $0.isEmpty }) { try await Task.sleep(for: .milliseconds(10)) }
        #expect(HeldTodoServer.requestIDs == [first.idempotencyKey], "the app's replay has it in flight")

        // Done on a due alert meanwhile: it queues and asks for a replay
        await center.onAction?(.done, "2026-10-01", "0-b")
        #expect(HeldTodoServer.requestIDs == [first.idempotencyKey], "nothing sent beside the running replay")

        HeldTodoServer.hold.withLock { $0 = false }
        await replay.value
        let ids = HeldTodoServer.requestIDs
        let queued = try await cache.pendingOutbox()
        #expect(queued.isEmpty, "the action's write went out in the same replay, not at the next poll")
        #expect(ids.count == 2 && Set(ids).count == 2 && ids.first == first.idempotencyKey, "each write once: \(ids)")
        try await cache.wipe()
    }
}
