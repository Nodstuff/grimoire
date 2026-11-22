import Foundation
import Testing
@testable import TaisceKit

/// Review 2026-11-22 #14: the server's tie-safe turn paging (`seq`,
/// `before_id`, `after_seq`), used when it is there, and today's paging
/// when it is not (the production server before the upgrade).
@Suite struct TieSafePagingTests {
    static let key = FleetSessionKey(runtimeID: "r1", id: "s1")

    static func turnJSON(_ id: String, ended: Int, seq: Int?) -> String {
        let s = seq.map { #","seq":\#($0)"# } ?? ""
        return #"{"runtime_id":"r1","session_id":"s1","turn_id":"\#(id)","prompt":"p","reply":"r","started":\#(ended - 5),"ended":\#(ended)\#(s)}"#
    }

    static func page(_ turns: [String], more: Bool = false) -> MockServer.Reply {
        .json(#"{"turns":[\#(turns.joined(separator: ","))],"more":\#(more)}"#)
    }

    @Test func aTurnsSeqDecodesAndIsOptional() throws {
        let new = try JSONDecoder().decode(FabricTurn.self, from: Data(Self.turnJSON("a", ended: 10, seq: 7).utf8))
        let old = try JSONDecoder().decode(FabricTurn.self, from: Data(Self.turnJSON("a", ended: 10, seq: nil).utf8))
        #expect(new.seq == 7 && old.seq == nil)
        // the cache keeps it (the turn's own coding)
        #expect(try JSONDecoder().decode(FabricTurn.self, from: JSONEncoder().encode(new)).seq == 7)
    }

    /// A new server: `after_seq` answers in store order, late replays included.
    @Test func afterSeqOnANewServer() async throws {
        let server = MockServer { r in
            #expect(r.path == "/api/fabric/sessions/r1/s1/turns")
            #expect(r.query["after_seq"] == "4" && r.query["after"] == nil && r.query["before"] == nil)
            // seq 5 is a late replay of an old turn
            return Self.page([Self.turnJSON("late", ended: 3, seq: 5), Self.turnJSON("b", ended: 20, seq: 6)], more: true)
        }
        let page = try #require(try await server.client().turns(Self.key, afterSeq: 4, limit: 100))
        #expect(page.turns.map(\.turnID) == ["late", "b"] && page.more && page.topSeq == 6)
    }

    /// The production server today: it ignores `after_seq` and answers its
    /// newest page with no `seq`, or refuses it (400): either way nil, and
    /// the caller asks by time as before.
    @Test func afterSeqOnAnOldServerIsNil() async throws {
        let ignores = MockServer { _ in Self.page([Self.turnJSON("z", ended: 50, seq: nil), Self.turnJSON("y", ended: 40, seq: nil)], more: true) }
        #expect(try await ignores.client().turns(Self.key, afterSeq: 4, limit: 100) == nil)
        let refuses = MockServer { _ in MockServer.Reply(status: 400, chunks: [Data(#"{"error":"unknown parameter after_seq"}"#.utf8)]) }
        #expect(try await refuses.client().turns(Self.key, afterSeq: 4, limit: 100) == nil)
        let coded = MockServer { _ in MockServer.Reply(status: 400, chunks: [Data(#"{"error":"bad query","code":"bad_request"}"#.utf8)]) }
        #expect(try await coded.client().turns(Self.key, afterSeq: 4, limit: 100) == nil)
        // an empty answer is nothing new on either
        let empty = MockServer { _ in Self.page([]) }
        #expect(try await empty.client().turns(Self.key, afterSeq: 4, limit: 100)?.turns.isEmpty == true)
        // a real failure still fails
        let down = MockServer { _ in MockServer.Reply(status: 503, chunks: [Data("busy".utf8)], contentType: "text/plain") }
        await #expect(throws: APIError.self) { try await down.client().turns(Self.key, afterSeq: 4, limit: 100) }
    }

    /// `before` + `before_id` on a new server; a server that refuses
    /// `before_id` gets today's `before=` a second on.
    @Test func beforeIDAndItsFallback() async throws {
        let new = MockServer { r in
            #expect(r.query["before"] == "30" && r.query["before_id"] == "t30")
            return Self.page([Self.turnJSON("t29", ended: 30, seq: 3)])
        }
        #expect(try await new.client().turns(Self.key, before: 30, beforeID: "t30", limit: 30).turns.map(\.turnID) == ["t29"])
        let asked = Counter()
        let old = MockServer { r in
            if asked.next() == 0 {
                #expect(r.query["before_id"] == "t30")
                return MockServer.Reply(status: 400, chunks: [Data(#"{"error":"before_id needs before"}"#.utf8)])
            }
            #expect(r.query["before"] == "31" && r.query["before_id"] == nil)
            return Self.page([Self.turnJSON("t29", ended: 30, seq: nil)])
        }
        #expect(try await old.client().turns(Self.key, before: 30, beforeID: "t30", limit: 30).turns.map(\.turnID) == ["t29"])
        #expect(old.requests.count == 2)
    }

    @Test func aSeqPageIsNumberedAscendingAndAfterTheCursor() {
        func t(_ seq: Int?) -> FabricTurn { FabricTurn(runtimeID: "r1", sessionID: "s1", address: "", turnID: "\(seq ?? -1)", prompt: "", reply: "", started: 0, ended: 0, seq: seq) }
        #expect(FabricTurnPage(turns: [t(5), t(6)], more: false).isSeqPage(afterSeq: 4))
        #expect(!FabricTurnPage(turns: [t(4), t(6)], more: false).isSeqPage(afterSeq: 4), "at the cursor")
        #expect(!FabricTurnPage(turns: [t(6), t(5)], more: false).isSeqPage(afterSeq: 4), "newest first")
        #expect(!FabricTurnPage(turns: [t(nil)], more: false).isSeqPage(afterSeq: 4), "unnumbered")
        #expect(FabricTurnPage(turns: [], more: false).isSeqPage(afterSeq: 4))
    }

    /// The cache keeps a store-order watermark beside the time one: only
    /// forward, cleared with the session, nil for what a server without
    /// `seq` wrote.
    @Test func theCacheKeepsASeqWatermark() async throws {
        let cache = try Cache.inMemory()
        let a = FabricTurn(runtimeID: "r1", sessionID: "s1", address: "", turnID: "a", prompt: "", reply: "", started: 5, ended: 10, seq: 3)
        try await cache.cacheFabricTurns([a], syncedThrough: 10)
        #expect(try await cache.fabricTurns(Self.key).syncedSeq == nil, "no seq read yet")
        try await cache.cacheFabricTurns([a], syncedThrough: 10, syncedSeqThrough: 3)
        try await cache.cacheFabricTurns([a], syncedSeqThrough: 2)
        let held = try await cache.fabricTurns(Self.key)
        #expect(held.syncedSeq == 3 && held.synced == 10 && held.turns.first?.seq == 3)
        try await cache.replaceFabricTurns(Self.key, with: [a], older: true, syncedThrough: 10)
        #expect(try await cache.fabricTurns(Self.key).syncedSeq == nil, "a replaced session starts again")
        try await cache.dropFabricTurns([Self.key])
        #expect(try await cache.fabricTurns(Self.key).syncedSeq == nil)
    }
}
