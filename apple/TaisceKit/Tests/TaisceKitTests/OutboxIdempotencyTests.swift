import Foundation
import Testing
@testable import TaisceKit

/// Review 2026-11-22 #2: a to-do write goes out once, and a resend is one
/// the server can recognise.
@Suite struct OutboxIdempotencyTests {
    static let day = #"{"carried":0,"date":"2026-10-01","doc_id":"t","epoch":3,"items":[],"prev_date":null,"today":"2026-10-01"}"#
    static let clock = TodoClock(today: "2026-10-01", utcOffset: "+00:00")

    func body(_ r: URLRequest) throws -> [String: Any] {
        try #require(try JSONSerialization.jsonObject(with: r.httpBody ?? Data()) as? [String: Any])
    }

    /// Every to-do route carries the row's key as `request_id`, the same
    /// key on a resend after a lost answer.
    @Test func everyTodoWriteSendsItsKeyAsRequestID() async throws {
        let cache = try Cache.inMemory()
        let rows = [
            try await cache.enqueueTodoAdd(date: "2026-10-01", text: "pay rent", clock: Self.clock),
            try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: true, clock: Self.clock),
            try await cache.enqueueTodo("/api/todo/edit", date: "2026-10-01", itemID: "0-a", text: "pay", clock: Self.clock),
            try await cache.enqueueTodo("/api/todo/remove", date: "2026-10-01", itemID: "0-a", clock: Self.clock),
            try await cache.enqueueTodo("/api/todo/move", date: "2026-10-01", itemID: "0-a", toDate: "2026-10-02", clock: Self.clock),
            try await cache.enqueueTodo("/api/todo/note", date: "2026-10-01", itemID: "0-a", note: "n", clock: Self.clock),
            try await cache.enqueueDeadline(date: "2026-10-01", itemID: "0-a", deadline: nil as Deadline?, clock: Self.clock),
            try await cache.enqueueToggle(date: "2026-10-01", itemID: "0-b", done: false),
        ]
        // the first answer is lost (transport failure): it may have landed
        let calls = Counter()
        let server = MockServer { _ in calls.next() == 0 ? MockServer.Reply(chunks: [], failAfter: true) : .json(Self.day) }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        let sent = server.requests
        #expect(sent.map(\.url?.path) == ["/api/todo"] + rows.map(\.path))
        let ids = try sent.map { try body($0)["request_id"] as? String }
        #expect(ids == [rows[0].idempotencyKey] + rows.map(\.idempotencyKey), "the resend reuses the first try's key")
        #expect(ids.allSatisfy { $0.flatMap(UUID.init(uuidString:)) != nil })
        // the rest of the body is as queued (null deadline kept)
        let cleared = try body(sent[7])
        #expect(cleared["deadline"] is NSNull && cleared["item_id"] as? String == "0-a" && cleared["today"] as? String == "2026-10-01")
        #expect(try body(sent[8])["done"] as? Bool == false)
        #expect(try await cache.pendingOutbox().isEmpty)
    }

    /// A server without request_id support ignores the field and answers
    /// the day as before (no `replayed`): the write lands, nothing breaks.
    /// A new server's replay answer (`replayed: true`) settles the row too.
    @Test func oldAndNewServersBothSettleTheRow() async throws {
        for answer in [Self.day, Self.day.replacingOccurrences(of: "{\"carried\"", with: "{\"replayed\":true,\"carried\"")] {
            let cache = try Cache.inMemory()
            try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: true, clock: Self.clock)
            let server = MockServer { _ in .json(answer) }
            let report = try await OutboxReplayer(api: server.client(), cache: cache).replay()
            #expect(report.forbidden == 0 && report.gone == 0)
            #expect(try await cache.pendingOutbox().isEmpty)
            #expect(try await cache.failedOutbox().isEmpty)
        }
    }

    /// Only a UUID goes out as request_id; a body that already names one
    /// keeps it; doc writes are untouched.
    @Test func onlyAUUIDKeyIsSent() async throws {
        let cache = try Cache.inMemory()
        try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: true, clock: Self.clock, key: "todo-1")
        let server = MockServer { _ in .json(Self.day) }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(try body(try #require(server.requests.first))["request_id"] == nil)
        let e = OutboxEntry(id: nil, createdAt: .now, idempotencyKey: UUID().uuidString, method: "POST", path: "/api/todo",
                            body: Data(#"{"request_id":"mine","date":"d"}"#.utf8), state: .pending, attempts: 0)
        #expect(OutboxReplayer.withRequestID(e) == nil)
        var move = e
        move.path = "/api/doc/d1/move"
        move.body = Data("{}".utf8)
        #expect(OutboxReplayer.withRequestID(move) == nil)
    }

    /// A row another replay has in flight is never claimed again (it went
    /// out twice when the notification action's replay ran beside the
    /// app's); crash recovery puts it back once, at open.
    @Test func aRowInFlightIsNotClaimedTwiceAndARestartRecoversIt() async throws {
        let cache = try Cache.inMemory()
        let row = try await cache.enqueueTodoToggle(date: "2026-10-01", itemID: "0-a", done: true, clock: Self.clock)
        let id = try #require(row.id)
        #expect(try await cache.claimOutbox(id)?.state == .inflight)
        #expect(try await cache.claimOutbox(id) == nil, "the first replay still has it")
        let server = MockServer { _ in .json(Self.day) }
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(server.requests.isEmpty, "a second replay leaves it to the first")
        // the app was killed mid-send: the next launch releases it and it goes out
        #expect(try await cache.releaseInflight() == 1)
        try await OutboxReplayer(api: server.client(), cache: cache).replay()
        #expect(server.requests.count == 1)
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.releaseInflight() == 0)
    }
}
