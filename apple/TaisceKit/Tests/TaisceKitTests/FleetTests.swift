import Foundation
import Synchronization
import Testing
@testable import TaisceKit

/// Agent Fabric's fleet (Slice 1, Watch): the wire shapes as a real daemon
/// sent them (`FleetFixture`), how changes apply, and how the stream is followed.
@Suite struct FleetTests {
    static func decode<T: Decodable>(_ type: T.Type = T.self, _ json: String) throws -> T {
        try JSONDecoder().decode(T.self, from: FleetFixture.data(json))
    }

    /// Every output of the SSE parser for `text`.
    static func outputs(_ text: String) -> [SSEOutput] {
        var parser = SSEParser()
        return parser.feed(Data(text.utf8))
    }

    static func events(_ text: String) -> [SSEEvent] {
        outputs(text).compactMap { if case .event(let e) = $0 { e } else { nil } }
    }

    // MARK: the wire

    @Test func theFleetDecodesAsTheServerSendsIt() throws {
        let fleet = try Self.decode(Fleet.self, FleetFixture.live)
        #expect(fleet.rev == 9)
        // the server's order, kept: needs you, finished, asleep
        #expect(fleet.sessions.map(\.address) == ["taisce/main", "workbox/main", "qompass/mcp-dx"])
        #expect(fleet.sessions.map(\.state) == [.needsYou, .finished, .asleep])
        let asks = fleet.sessions[0]
        #expect(asks.key == FleetSessionKey(runtimeID: FleetFixture.runtime, id: FleetFixture.taisce))
        #expect(asks.lastLine == "The tests pass. Shall I open the pull request?" && asks.tool.isEmpty && asks.note.isEmpty)
        #expect(asks.account == "personal" && asks.branch == "agent-fabric" && asks.fabric == "runtime-channel")
        #expect(asks.contextUsedPct == 44 && asks.since == 1_795_213_864 && asks.quietUntil == nil)
        // unknown is null, not zero
        #expect(fleet.sessions[2].contextUsedPct == nil)
        let mac = try #require(fleet.runtimes.first)
        #expect(mac.name == "mbp" && mac.online && mac.lastSeenAt == 1_795_213_871 && mac.version == "fake/0.8.0")
        #expect(mac.accounts == [
            FleetAccount(name: "personal", sessionUsedPct: 38, weekUsedPct: 61),
            FleetAccount(name: "work", sessionUsedPct: 92, weekUsedPct: 70),
        ])
        #expect(mac.accounts[0].sessionResetsAt == nil && mac.accounts[0].weekResetsAt == nil)
        #expect(fleet.attentionCount(at: .distantPast) == 1)
    }

    @Test func aRuntimeThatNeverConnectedAndOneThatWentAway() throws {
        let fresh = try Self.decode(Fleet.self, FleetFixture.neverConnected)
        #expect(fresh.sessions.isEmpty && fresh.runtimes.count == 1)
        #expect(fresh.runtimes[0].online == false && fresh.runtimes[0].lastSeenAt == nil && fresh.runtimes[0].accounts.isEmpty && fresh.runtimes[0].accountsAt == nil)

        let away = try Self.decode(Fleet.self, FleetFixture.offline)
        #expect(away.runtimes[0].online == false && away.runtimes[0].lastSeenAt == 1_795_213_871)
        // its sessions are still listed, as last known; none of them counts as needing you now
        #expect(away.sessions.count == 3 && away.attentionCount() == 0)
        let snoozed = away.sessions[2]
        #expect(snoozed.quietUntil == 1_795_217_471)
        #expect(snoozed.isSnoozed(at: Date(timeIntervalSince1970: 1_795_217_470)))
        #expect(!snoozed.isSnoozed(at: Date(timeIntervalSince1970: 1_795_217_471)), "a time that has passed is no snooze")
        #expect(!away.sessions[0].isSnoozed(at: .distantPast))
    }

    @Test func aStateThisBuildHasNoNameForIsUnknownAndAnOlderServerHasNoSnooze() throws {
        // typed: the recorded session with a state a newer runtime might report, and no quiet_until
        let json = FleetFixture.unsnoozed
            .replacingOccurrences(of: #""state":"finished""#, with: #""state":"reticulating""#)
            .replacingOccurrences(of: #","quiet_until":null"#, with: "")
        let s = try Self.decode(FleetSession.self, json)
        #expect(s.state == .unknown && s.quietUntil == nil && s.address == "workbox/main")
        #expect(!SessionState.unknown.isResting && !SessionState.unknown.wantsAttention)
        #expect(SessionState.allCases.filter(\.isResting) == [.idle, .asleep, .exited])
        #expect(SessionState.allCases.filter(\.wantsAttention) == [.needsYou, .stalled])
    }

    // MARK: the routes

    @Test func theRoutesAreAskedAsTheServerServesThem() async throws {
        let server = MockServer { r in
            switch (r.httpMethod, r.path) {
            case ("GET", "/api/fabric/sessions"): .json(FleetFixture.live)
            case ("GET", "/api/fabric/runtimes"): .json(FleetFixture.runtimes)
            case ("POST", "/api/fabric/sessions/\(FleetFixture.runtime)/\(FleetFixture.workbox)/snooze"): .json(FleetFixture.snoozed)
            case ("DELETE", "/api/fabric/sessions/\(FleetFixture.runtime)/\(FleetFixture.workbox)/snooze"): .json(FleetFixture.unsnoozed)
            default: MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found"}"#.utf8)])
            }
        }
        let api = server.client()
        #expect(try await api.fleet().sessions.count == 3)
        #expect(try await api.fleetRuntimes().map(\.name) == ["mbp"])
        let key = FleetSessionKey(runtimeID: FleetFixture.runtime, id: FleetFixture.workbox)
        let until = Date(timeIntervalSince1970: 1_795_217_470.2)
        let snoozed = try await api.snoozeSession(key, until: until)
        #expect(snoozed.quietUntil == 1_795_217_471 && snoozed.key == key)
        let post = try #require(server.requests.first { $0.httpMethod == "POST" })
        let body = try JSONSerialization.jsonObject(with: try #require(post.httpBody)) as? [String: Int]
        #expect(body == ["until": 1_795_217_471], "whole seconds, never earlier than asked")
        #expect(try await api.unsnoozeSession(key).quietUntil == nil)
        // a session that is not yours, or gone
        await #expect(throws: APIError.notFound("not found")) {
            try await api.unsnoozeSession(FleetSessionKey(runtimeID: FleetFixture.runtime, id: "nope"))
        }
    }

    @Test func aLocalDaemonHasNoFleet() async throws {
        // LOCAL mode: every fabric route is a 404 `{"error":"not found"}`
        let server = MockServer { _ in MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found"}"#.utf8)]) }
        await #expect(throws: APIError.notFound("not found")) { try await server.client().fleet() }
    }

    // MARK: the stream

    @Test func theRecordedStreamReplaysIntoTheFleetTheServerThenServed() throws {
        let outputs = Self.outputs(FleetFixture.stream)
        #expect(outputs.first == .retry(3000))
        var fleet = Fleet()
        var names: [String] = []
        for event in Self.events(FleetFixture.stream) {
            let (change, rev) = try #require(try FleetChange.decode(event))
            names.append(event.event)
            let applied1 = fleet.apply(change, rev: rev)
            #expect(applied1)
            #expect(fleet.rev == rev)
        }
        #expect(names == ["fleet", "runtime", "session", "session", "session", "runtime", "session", "session", "session", "session", "session", "session", "runtime"])
        // what `GET /api/fabric/sessions` answered once the stream had said all that
        #expect(fleet == (try Self.decode(Fleet.self, FleetFixture.offline)))
    }

    @Test func aChangeKeepsARowsPlaceAndAnOlderOneIsIgnored() throws {
        var fleet = try Self.decode(Fleet.self, FleetFixture.live)
        // workbox/main (finished, second) goes back to work: it stays second
        var working = fleet.sessions[1]
        working.state = .working
        let applied2 = fleet.apply(.session(working), rev: 10)
        #expect(applied2)
        #expect(fleet.sessions.map(\.address) == ["taisce/main", "workbox/main", "qompass/mcp-dx"])
        #expect(fleet.sessions[1].state == .working && fleet.rev == 10)
        // a session never seen goes last, whatever it needs
        let new = FleetSession(sessionID: "s9", runtimeID: FleetFixture.runtime, address: "notes/main", state: .needsYou, since: 1, updatedAt: 1)
        let applied3 = fleet.apply(.session(new), rev: 11)
        #expect(applied3)
        #expect(fleet.sessions.map(\.address).last == "notes/main")
        // an event at or below the rev held is older than the fleet
        var stale = working
        stale.state = .exited
        let applied4 = fleet.apply(.session(stale), rev: 11)
        #expect(!applied4)
        #expect(fleet.sessions[1].state == .working)
        // the same id under another runtime is another session
        let applied5 = fleet.apply(.sessionGone(FleetSessionKey(runtimeID: "other", id: FleetFixture.taisce)), rev: 12)
        #expect(applied5)
        #expect(fleet.sessions.count == 4)
        let applied6 = fleet.apply(.sessionGone(FleetSessionKey(runtimeID: FleetFixture.runtime, id: FleetFixture.taisce)), rev: 13)
        #expect(applied6)
        #expect(fleet.sessions.map(\.address) == ["workbox/main", "qompass/mcp-dx", "notes/main"])
        // a runtime removed takes its sessions with it
        let applied7 = fleet.apply(.runtimeGone(id: FleetFixture.runtime), rev: 14)
        #expect(applied7)
        #expect(fleet.runtimes.isEmpty && fleet.sessions.isEmpty && fleet.rev == 14)
    }

    @Test func aRefreshBringsTheServersOrderBackUnlessTheStreamIsPastIt() throws {
        var fleet = try Self.decode(Fleet.self, FleetFixture.live)
        var later = fleet
        later.rev = 12
        later.sessions.reverse()
        let applied8 = fleet.refresh(with: later)
        #expect(applied8)
        #expect(fleet.sessions.map(\.address) == ["qompass/mcp-dx", "workbox/main", "taisce/main"])
        // an answer older than what the stream has said is dropped
        let applied9 = fleet.refresh(with: try Self.decode(Fleet.self, FleetFixture.live))
        #expect(!applied9)
        #expect(fleet.rev == 12)
        // the stream's own fleet always replaces (a restarted daemon counts from 1 again)
        let applied10 = fleet.apply(.fleet(try Self.decode(Fleet.self, FleetFixture.neverConnected)), rev: 1)
        #expect(applied10)
        #expect(fleet.rev == 1 && fleet.sessions.isEmpty)
    }

    @Test func goneEventsAndEventsThisBuildDoesNotKnow() throws {
        let removed = try #require(Self.events(FleetFixture.runtimeGone).first)
        let gone = try #require(try FleetChange.decode(removed))
        #expect(gone.change == .runtimeGone(id: "01a1fb16-29a2-75d1-9e5a-e9bdfc58ed09") && gone.rev == 15)
        let evicted = try #require(Self.events(FleetFixture.sessionGone).first)
        let session = try #require(try FleetChange.decode(evicted))
        #expect(session.change == .sessionGone(FleetSessionKey(runtimeID: FleetFixture.runtime, id: FleetFixture.workbox)) && session.rev == 14)
        // a newer server's event is skipped; a known one that cannot be read is an error
        #expect(try FleetChange.decode(SSEEvent(id: "20", event: "turn", data: "{}")) == nil)
        #expect(throws: (any Error).self) { try FleetChange.decode(SSEEvent(id: "20", event: "session", data: #"{"id":"x"}"#)) }
        #expect(throws: (any Error).self) { try FleetChange.decode(SSEEvent(id: nil, event: "runtime_gone", data: #"{"id":"x"}"#)) }
    }

    // MARK: following

    /// Every signal a follower emits until its `stopAfter`th wait.
    static func follow(_ server: MockServer, stopAfter: Int) async -> (signals: [FleetSignal], sleeps: [Duration]) {
        let sleeps = Sleeps(stopAfter: stopAfter)
        let follower = FleetFollower(
            service: server.client(),
            backoff: Backoff(base: .seconds(1), cap: .seconds(30), jitter: { 1 }),
            sleep: { try await sleeps.sleep($0) }
        )
        var out: [FleetSignal] = []
        for await s in follower.signals() { out.append(s) }
        return (out, sleeps.recorded)
    }

    static func links(_ signals: [FleetSignal]) -> [FleetLink] {
        signals.compactMap { if case .link(let l) = $0 { l } else { nil } }
    }

    static func changes(_ signals: [FleetSignal]) -> [FleetChange] {
        signals.compactMap { if case .change(let c, _) = $0 { c } else { nil } }
    }

    @Test func followingAppliesTheStreamAndReconnectsForAFreshFleet() async throws {
        let calls = Mutex(0)
        let server = MockServer { r in
            #expect(r.path == "/api/fabric/stream")
            #expect(r.value(forHTTPHeaderField: "Accept") == "text/event-stream")
            #expect(r.value(forHTTPHeaderField: "Last-Event-ID") == nil, "the fleet's stream has no cursor")
            let n = calls.withLock { $0 += 1; return $0 }
            // the first connection is the recording; the server then ends it, and the next opens with the fleet again
            return n == 1 ? .sse(FleetFixture.stream) : .sse(": ping\n\n", "id: 1\nevent: fleet\ndata: \(FleetFixture.neverConnected)\n\n")
        }
        let (signals, sleeps) = await Self.follow(server, stopAfter: 2)
        #expect(Self.links(signals) == [.connecting, .live, .waiting(retryIn: .seconds(3)), .connecting, .live, .waiting(retryIn: .seconds(3)), .idle])
        // the server's `retry: 3000` is the floor under a 1 s backoff
        #expect(sleeps == [.seconds(3), .seconds(3)])
        var fleet = Fleet()
        for case .change(let change, let rev) in signals { fleet.apply(change, rev: rev) }
        #expect(fleet == (try Self.decode(Fleet.self, FleetFixture.neverConnected)), "the new connection's fleet replaced the old")
        #expect(Self.changes(signals).count == 14)
    }

    @Test func aServerWithNoFleetEndsTheFollowingAtOnce() async throws {
        let server = MockServer { _ in MockServer.Reply(status: 404, chunks: [Data(#"{"error":"not found"}"#.utf8)]) }
        let (signals, sleeps) = await Self.follow(server, stopAfter: 5)
        #expect(Self.links(signals) == [.connecting, .unavailable])
        #expect(sleeps.isEmpty && server.requests.count == 1)
        // an older daemon answers an unknown route with its web page
        let web = MockServer { _ in MockServer.Reply(chunks: [Data("<html></html>".utf8)], contentType: "text/html") }
        #expect(Self.links(await Self.follow(web, stopAfter: 5).signals) == [.connecting, .unavailable])
    }

    @Test func failuresBackOffAndAHealthyConnectionStartsOver() async throws {
        let calls = Mutex(0)
        let server = MockServer { _ in
            let n = calls.withLock { $0 += 1; return $0 }
            switch n {
            case 1, 2, 3: return MockServer.Reply(status: 503, chunks: [])
            // an event that cannot be read ends the connection: the next one brings a whole fleet
            case 4: return .sse("id: 1\nevent: fleet\ndata: \(FleetFixture.live)\n\n", "id: 2\nevent: session\ndata: {\"id\":1}\n\n")
            default: return MockServer.Reply(status: 503, chunks: [])
            }
        }
        let (signals, sleeps) = await Self.follow(server, stopAfter: 5)
        #expect(sleeps == [.seconds(1), .seconds(2), .seconds(4), .seconds(1), .seconds(2)], "doubling, then from the start after a connection that delivered")
        let failures = signals.filter { if case .failed = $0 { true } else { false } }
        #expect(failures.count == 5)
        #expect(Self.changes(signals).count == 1)
        #expect(Self.links(signals).last == .idle)
    }

    @Test func aStreamThatOpensWithSomethingUnreadableBacksOff() async throws {
        // a 200 whose first event cannot be read, every time: never "healthy", so the wait grows
        let server = MockServer { _ in .sse("retry: 10\n\n", ": ping\n\n", "id: 1\nevent: fleet\ndata: {\"rev\":\"one\"}\n\n") }
        let (signals, sleeps) = await Self.follow(server, stopAfter: 4)
        #expect(sleeps == [.seconds(1), .seconds(2), .seconds(4), .seconds(8)])
        #expect(Self.changes(signals).isEmpty && !Self.links(signals).contains(.live))
    }

    @Test func aSessionThatIsOverEndsTheFollowing() async throws {
        // 401, with no token to renew: the following ends; it is not retried
        let server = MockServer { _ in MockServer.Reply(status: 401, chunks: [Data(#"{"error":"invalid_token","code":"unauthorized"}"#.utf8)]) }
        let (signals, sleeps) = await Self.follow(server, stopAfter: 5)
        #expect(Self.links(signals) == [.connecting, .idle])
        #expect(sleeps.isEmpty && server.requests.count == 1)
        let failed = signals.contains { if case .failed(let e) = $0 { (e as? APIError) == .unauthorized } else { false } }
        #expect(failed)
    }

    @Test func aSnoozedSessionDoesNotCountAsNeedingYou() throws {
        var fleet = try Self.decode(Fleet.self, FleetFixture.live)
        let now = Date(timeIntervalSince1970: 1_795_213_900)
        #expect(fleet.attentionCount(at: now) == 1)
        fleet.sessions[0].quietUntil = 1_795_217_471
        #expect(fleet.attentionCount(at: now) == 0 && fleet.sessions.count == 3, "still listed, no longer counted")
        // once its time has passed it counts again
        #expect(fleet.attentionCount(at: Date(timeIntervalSince1970: 1_795_217_471)) == 1)
    }
}
