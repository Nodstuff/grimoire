import Foundation
import Testing
@testable import TaisceKit

/// Agent Fabric turns kept per session in the doc cache (`Cache+FabricTurns`).
@Suite struct FabricTurnCacheTests {
    let s1 = FleetSessionKey(runtimeID: "r1", id: "s1")
    let s2 = FleetSessionKey(runtimeID: "r1", id: "s2")

    func turn(_ id: String, _ ended: Int, session: String = "s1", runtime: String = "r1", reply: String = "r", textOff: Bool = false) -> FabricTurn {
        FabricTurn(runtimeID: runtime, sessionID: session, address: "taisce/main", turnID: id, prompt: "p \(id)", reply: reply,
                   toolCount: 2, started: ended - 5, ended: ended, end: .question, source: .order, textOff: textOff)
    }

    @Test func turnsAreKeptPerSessionOldestFirstAndRoundTrip() async throws {
        let cache = try Cache.inMemory()
        let t = turn("a", 30)
        try await cache.cacheFabricTurns([turn("c", 50), t, turn("b", 40), turn("x", 45, session: "s2")])
        let held = try await cache.fabricTurns(s1)
        #expect(held.turns.map(\.turnID) == ["a", "b", "c"])
        #expect(held.turns[0] == t, "every field comes back")
        #expect(held.older, "not known: Earlier turns stays offered")
        #expect(try await cache.fabricTurns(s2).turns.map(\.turnID) == ["x"])
        // append: more turns join what is held
        try await cache.cacheFabricTurns([turn("d", 60)])
        #expect(try await cache.fabricTurns(s1).turns.map(\.turnID) == ["a", "b", "c", "d"])
    }

    @Test func aTurnIsKeptOnceAndTheFullerCopyWins() async throws {
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns([turn("a", 30, reply: "the whole reply")])
        try await cache.cacheFabricTurns([turn("a", 30, reply: ""), turn("a", 30, reply: "the")])
        let held = try await cache.fabricTurns(s1).turns
        #expect(held.count == 1 && held[0].reply == "the whole reply")
        try await cache.cacheFabricTurns([turn("a", 30, reply: "the whole reply, and more")])
        #expect(try await cache.fabricTurns(s1).turns[0].reply == "the whole reply, and more")
    }

    @Test func eachSessionKeepsItsNewestHundred() async throws {
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns((1...60).map { turn("t\($0)", $0) }, olderBeyond: false)
        #expect(try await cache.fabricTurns(s1).older == false)
        try await cache.cacheFabricTurns((61...130).map { turn("t\($0)", $0) })
        try await cache.cacheFabricTurns([turn("other", 1, session: "s2")])
        let held = try await cache.fabricTurns(s1)
        #expect(held.turns.count == Cache.fabricTurnsKept)
        #expect(held.turns.first?.turnID == "t31" && held.turns.last?.turnID == "t130")
        #expect(held.older, "what was let go is on the server")
        #expect(try await cache.fabricTurns(s2).turns.count == 1, "another session's turns are its own")
    }

    @Test func olderIsRecordedOnlyFromAPageReachingBackAsFar() async throws {
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns((10...20).map { turn("t\($0)", $0) }, olderBeyond: true)
        // a newer page's `more` says nothing about what lies before the oldest held
        try await cache.cacheFabricTurns([turn("t30", 30)], olderBeyond: false)
        #expect(try await cache.fabricTurns(s1).older)
        try await cache.cacheFabricTurns([turn("t5", 5)], olderBeyond: false)
        #expect(try await cache.fabricTurns(s1).older == false)
    }

    /// The contiguity watermark: only a route read moves it, it never goes
    /// back, and a stream turn held above it does not pass it (what ended in
    /// between may be missing: the top-up asks from the watermark).
    @Test func theWatermarkMovesOnlyForwardAndOnlyFromARouteRead() async throws {
        let cache = try Cache.inMemory()
        #expect(try await cache.fabricTurns(s1).synced == nil, "nothing held")
        try await cache.cacheFabricTurns((1...3).map { turn("t\($0)", $0 * 10) }, olderBeyond: false, syncedThrough: 30)
        #expect(try await cache.fabricTurns(s1).synced == 30)
        // the stream's turn, after a hole (t4 ended while the app was away)
        try await cache.cacheFabricTurns([turn("t5", 50)])
        let held = try await cache.fabricTurns(s1)
        #expect(held.turns.map(\.turnID) == ["t1", "t2", "t3", "t5"] && held.synced == 30, "a stream turn never moves it")
        // an older page (`before=`) says nothing newer: an earlier mark is ignored
        try await cache.cacheFabricTurns([turn("t0", 5)], olderBeyond: false, syncedThrough: 5)
        #expect(try await cache.fabricTurns(s1).synced == 30)
        try await cache.cacheFabricTurns([turn("t4", 40), turn("t5", 50)], syncedThrough: 50)
        #expect(try await cache.fabricTurns(s1).synced == 50)
        // replaced (an old server's page): the mark is the page's
        try await cache.replaceFabricTurns(s1, with: [turn("t9", 90)], older: true, syncedThrough: 90)
        #expect(try await cache.fabricTurns(s1).synced == 90)
        // dropped and wiped with the turns
        try await cache.dropFabricTurns([s1])
        try await cache.cacheFabricTurns([turn("t10", 100)])
        #expect(try await cache.fabricTurns(s1).synced == nil, "a session dropped starts again unknown")
    }

    @Test func textOffTurnsAreKeptWithoutTheirWords() async throws {
        let cache = try Cache.inMemory()
        // the live event carries the words; what is written never does
        try await cache.cacheFabricTurns([turn("a", 30, reply: "zebra secret", textOff: true)])
        let held = try await cache.fabricTurns(s1).turns
        #expect(held.count == 1 && held[0].prompt.isEmpty && held[0].reply.isEmpty && held[0].textOff && held[0].toolCount == 2)
        // turned Text off later: a copy held with words loses them, to the route's blank or by blanking
        try await cache.cacheFabricTurns([turn("b", 40, reply: "kept before"), turn("c", 50, reply: "also kept")])
        try await cache.cacheFabricTurns([FabricTurn(runtimeID: "r1", sessionID: "s1", address: "taisce/main", turnID: "b", prompt: "", reply: "",
                                                     started: 35, ended: 40, textOff: true)])
        try await cache.blankFabricTurns([s1])
        let after = try await cache.fabricTurns(s1).turns
        #expect(after.allSatisfy { $0.prompt.isEmpty && $0.reply.isEmpty }, "\(after.map(\.reply))")
    }

    @Test func sessionsThatAreGoneTakeTheirTurns() async throws {
        let cache = try Cache.inMemory()
        try await cache.cacheFabricTurns([turn("a", 1), turn("b", 2, session: "s2"), turn("c", 3, session: "s3"), turn("d", 4, session: "s1", runtime: "r2")])
        try await cache.dropFabricTurns([s2])
        #expect(try await cache.fabricTurns(s2).turns.isEmpty)
        try await cache.dropFabricTurns(keeping: [s1])
        #expect(try await cache.fabricTurnCount() == 1)
        #expect(try await cache.fabricTurns(s1).turns.map(\.turnID) == ["a"])
    }

    @Test func aWipeForgetsTurnsAndTheyCountAsCachedData() async throws {
        let cache = try Cache.inMemory()
        #expect(try await cache.isEmpty())
        try await cache.cacheFabricTurns([turn("a", 1)])
        #expect(try await cache.isEmpty() == false, "a fresh sign-in over unclaimed turns wipes them (CacheOwnership)")
        try await cache.wipe()
        #expect(try await cache.fabricTurnCount() == 0)
    }

    @Test func anotherPersonSigningInGetsNoTurns() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "turns-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let path = dir.appending(path: "cache.sqlite").path
        do {
            let cache = try Cache(path: path)
            try await cache.setOwner("p-tom")
            try await cache.cacheFabricTurns([turn("a", 1, reply: "zebrasecret")])
        }
        let cache = try Cache(path: path)
        #expect(try await cache.fabricTurns(s1).turns.count == 1, "the same person: kept across launches")
        let d = CacheOwnership.decide(cached: try await cache.owner(), current: "p-ann", freshSignIn: true, cacheEmpty: try await cache.isEmpty())
        #expect(d == .wipe(owner: "p-ann"))
        try await CacheOwnership.apply(d, to: cache)
        #expect(try await cache.fabricTurnCount() == 0)
        let bytes = try Data(contentsOf: URL(fileURLWithPath: path))
        #expect(bytes.range(of: Data("zebrasecret".utf8)) == nil, "secure_delete and vacuum leave nothing on disk")
    }
}
