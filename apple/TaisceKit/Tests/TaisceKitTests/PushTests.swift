import Foundation
import Synchronization
import Testing
@testable import TaisceKit

/// A `PushRegistrationStore` in memory.
final class MemoryPushStore: PushRegistrationStore {
    private let value = Mutex<SentPushRegistration?>(nil)
    func load() -> SentPushRegistration? { value.withLock { $0 } }
    func save(_ sent: SentPushRegistration?) { value.withLock { $0 = sent } }
}

/// A settable clock.
final class TestClock: Sendable {
    private let value: Mutex<Date>
    init(_ d: Date) { value = Mutex(d) }
    var now: Date { value.withLock { $0 } }
    func advance(_ s: TimeInterval) { value.withLock { $0 += s } }
}

@Suite struct PushTokenTests {
    @Test func hexIsLowercaseTwoDigitsPerByte() {
        #expect(PushToken.hex(Data([0x00, 0x0f, 0xa0, 0xff, 0x12])) == "000fa0ff12")
        #expect(PushToken.hex(Data()) == "")
        let token = Data((0..<32).map { UInt8($0 * 8) })
        let hex = PushToken.hex(token)
        #expect(hex.count == 64 && hex == hex.lowercased())
        #expect(hex == token.map { String(format: "%02x", $0) }.joined())
    }

    @Test func registrationEncodesTheServerShape() throws {
        let r = DeviceRegistration(token: "ab01", env: .sandbox, appVersion: "0.1.0 (1)")
        let obj = try JSONSerialization.jsonObject(with: JSONEncoder().encode(r)) as? [String: String]
        #expect(obj == ["token": "ab01", "platform": "ios", "env": "sandbox", "app_version": "0.1.0 (1)"])
    }

    @Test func resendDecision() {
        let t0 = Date(timeIntervalSince1970: 1_790_000_000)
        let r = DeviceRegistration(token: "aa", env: .production, appVersion: "1")
        let last = SentPushRegistration(server: "https://s", registration: r, sentAt: t0)
        #expect(SentPushRegistration.shouldSend(r, to: "https://s", last: nil, now: t0))
        #expect(!SentPushRegistration.shouldSend(r, to: "https://s", last: last, now: t0 + 3600))
        #expect(!SentPushRegistration.shouldSend(r, to: "https://s", last: last, now: t0 + 86_399))
        #expect(SentPushRegistration.shouldSend(r, to: "https://s", last: last, now: t0 + 86_400), "a day old")
        #expect(SentPushRegistration.shouldSend(r, to: "https://s", last: last, now: t0 - 60), "clock went backwards")
        #expect(SentPushRegistration.shouldSend(r, to: "https://other", last: last, now: t0 + 1), "another server")
        var changed = r
        changed.token = "bb"
        #expect(SentPushRegistration.shouldSend(changed, to: "https://s", last: last, now: t0 + 1), "new token")
        changed = r
        changed.env = .sandbox
        #expect(SentPushRegistration.shouldSend(changed, to: "https://s", last: last, now: t0 + 1), "new environment")
        changed = r
        changed.appVersion = "2"
        #expect(SentPushRegistration.shouldSend(changed, to: "https://s", last: last, now: t0 + 1), "new app version")
    }

    @Test func userDefaultsStoreRoundTrips() throws {
        let suite = "push-test-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        defer { defaults.removePersistentDomain(forName: suite) }
        let store = UserDefaultsPushStore(defaults: defaults)
        #expect(store.load() == nil)
        let sent = SentPushRegistration(server: "https://s", registration: DeviceRegistration(token: "aa", env: .sandbox, appVersion: "1"), sentAt: Date(timeIntervalSince1970: 1_790_000_000))
        store.save(sent)
        #expect(store.load() == sent)
        store.save(nil)
        #expect(store.load() == nil)
    }
}

@Suite struct PushRegistrarTests {
    static let t0 = Date(timeIntervalSince1970: 1_790_000_000)

    /// A mock server answering `/api/devices` like the daemon.
    static func server(deleteStatus: Int = 200) -> MockServer {
        MockServer { r in
            switch (r.httpMethod, r.path) {
            case ("POST", "/api/devices"): return .json(#"{"ok":true}"#)
            case ("DELETE", _) where r.path.hasPrefix("/api/devices/"):
                if deleteStatus == 404 { return MockServer.Reply(status: 404, chunks: [Data(#"{"error":"unknown device"}"#.utf8)]) }
                return MockServer.Reply(status: deleteStatus, chunks: [Data(#"{"ok":true}"#.utf8)])
            default: return MockServer.Reply(status: 404, chunks: [Data(#"{"error":"no route"}"#.utf8)])
            }
        }
    }

    func registrar(_ store: MemoryPushStore, _ clock: TestClock, env: PushEnvironment = .sandbox) -> PushRegistrar {
        PushRegistrar(store: store, environment: env, appVersion: "0.1.0 (1)", now: { clock.now })
    }

    @Test func sendsOnceTheTokenAndServerAreBothKnown() async throws {
        let server = Self.server()
        let store = MemoryPushStore()
        let push = registrar(store, TestClock(Self.t0))
        #expect(await push.didRegister(deviceToken: Data([0xde, 0xad, 0xbe, 0xef])) == .waiting, "not signed in yet")
        #expect(server.requests.isEmpty)
        #expect(await push.connect(server: "https://s", registry: server.client()) == .sent)
        let r = try #require(server.requests.first)
        #expect(r.httpMethod == "POST" && r.path == "/api/devices")
        #expect(r.value(forHTTPHeaderField: "Content-Type") == "application/json")
        let body = try JSONSerialization.jsonObject(with: try #require(r.httpBody)) as? [String: String]
        #expect(body == ["token": "deadbeef", "platform": "ios", "env": "sandbox", "app_version": "0.1.0 (1)"])
        #expect(store.load()?.registration.token == "deadbeef" && store.load()?.server == "https://s")
    }

    @Test func resendsOnANewTokenAndDailyOtherwise() async throws {
        let server = Self.server()
        let store = MemoryPushStore()
        let clock = TestClock(Self.t0)
        let push = registrar(store, clock, env: .production)
        await push.connect(server: "https://s", registry: server.client())
        #expect(await push.didRegister(token: "aa") == .sent)
        // every launch delivers the token again: within the day, nothing goes out
        clock.advance(3600)
        #expect(await push.didRegister(token: "aa") == .upToDate)
        #expect(server.requests.count == 1)
        // a new token goes out at once
        #expect(await push.didRegister(token: "bb") == .sent)
        #expect(server.requests.count == 2)
        // and the same one again after a day
        clock.advance(86_400)
        #expect(await push.didRegister(token: "bb") == .sent)
        #expect(server.requests.count == 3)
        let body = try JSONSerialization.jsonObject(with: try #require(server.requests.last?.httpBody)) as? [String: String]
        #expect(body?["token"] == "bb" && body?["env"] == "production")
    }

    @Test func aFailedSendIsRetriedNextTime() async throws {
        let down = MockServer { _ in MockServer.Reply(status: 503, chunks: [Data("busy".utf8)], contentType: "text/plain") }
        let store = MemoryPushStore()
        let push = registrar(store, TestClock(Self.t0))
        await push.didRegister(token: "aa")
        guard case .failed = await push.connect(server: "https://s", registry: down.client()) else {
            Issue.record("a 503 is a failure")
            return
        }
        #expect(store.load() == nil, "nothing recorded as sent")
        let up = Self.server()
        #expect(await push.connect(server: "https://s", registry: up.client()) == .sent)
    }

    @Test func signOutDeletesTheSentToken() async throws {
        let server = Self.server()
        let store = MemoryPushStore()
        let push = registrar(store, TestClock(Self.t0))
        await push.didRegister(token: "aa01")
        await push.connect(server: "https://s", registry: server.client())
        #expect(await push.unregister() == .sent)
        let r = try #require(server.requests.last)
        #expect(r.httpMethod == "DELETE" && r.path == "/api/devices/aa01")
        #expect(store.load() == nil)
        // signed out: a token delivered now waits for the next sign-in
        #expect(await push.didRegister(token: "aa01") == .waiting)
        #expect(server.requests.count == 2)
        // which sends it again, though it is under a day old
        #expect(await push.connect(server: "https://s", registry: server.client()) == .sent)
    }

    @Test func anUnknownTokenOnDeleteIsAlreadyGone() async throws {
        let server = Self.server(deleteStatus: 404)
        try await server.client().unregisterDevice(token: "aa")
        let push = registrar(MemoryPushStore(), TestClock(Self.t0))
        await push.didRegister(token: "aa")
        await push.connect(server: "https://s", registry: server.client())
        #expect(await push.unregister() == .sent)
    }

    @Test func signOutWithNothingSentSendsNothing() async throws {
        let server = Self.server()
        let push = registrar(MemoryPushStore(), TestClock(Self.t0))
        await push.connect(server: "https://s", registry: server.client())
        #expect(await push.unregister() == .waiting)
        #expect(server.requests.isEmpty)
    }

    @Test func aServerSwitchSendsToTheNewServerAndOnlyItDeletes() async throws {
        let a = Self.server()
        let b = Self.server()
        let store = MemoryPushStore()
        let push = registrar(store, TestClock(Self.t0))
        await push.didRegister(token: "aa")
        await push.connect(server: "https://a", registry: a.client())
        #expect(await push.connect(server: "https://b", registry: b.client()) == .sent)
        #expect(b.requests.count == 1)
        #expect(await push.unregister() == .sent)
        #expect(b.requests.last?.httpMethod == "DELETE")
        #expect(a.requests.count == 1, "a's bearer is not b's")
    }
}

@Suite struct BackgroundRefreshTests {
    /// Counts reconciles.
    final class Counter: Sendable {
        private let n = Mutex(0)
        func bump() { n.withLock { $0 += 1 } }
        var value: Int { n.withLock { $0 } }
    }

    @Test func mapsTheCatchUpToTheFetchResult() async {
        let reconciled = Counter()
        #expect(await BackgroundRefresh.run(limit: .seconds(5), catchUp: { true }, reconcile: { reconciled.bump() }) == .newData)
        #expect(await BackgroundRefresh.run(limit: .seconds(5), catchUp: { false }, reconcile: { reconciled.bump() }) == .noData)
        #expect(await BackgroundRefresh.run(limit: .seconds(5), catchUp: { throw APIError.http(status: 503) }, reconcile: { reconciled.bump() }) == .failed)
        #expect(reconciled.value == 3, "alerts are reconciled whatever the catch-up did")
    }

    @Test func aHangingCatchUpIsCutOffAtTheLimit() async {
        let reconciled = Counter()
        let start = ContinuousClock.now
        // a catch-up that sleeps far past the limit (cancellable: it is cancelled)
        let result = await BackgroundRefresh.run(limit: .milliseconds(200), catchUp: {
            try await Task.sleep(for: .seconds(30))
            return true
        }, reconcile: { reconciled.bump() })
        let elapsed = ContinuousClock.now - start
        #expect(result == .failed)
        #expect(reconciled.value == 1)
        #expect(elapsed < .seconds(5), "returned in \(elapsed)")
    }

    @Test func aCatchUpThatIgnoresCancellationIsCutOffToo() async {
        let start = ContinuousClock.now
        // detached work isn't cancelled with its parent: the limit alone ends the wait
        let result = await BackgroundRefresh.run(limit: .milliseconds(200), catchUp: {
            await Task.detached { try? await Task.sleep(for: .seconds(3)) }.value
            return true
        }, reconcile: {})
        let elapsed = ContinuousClock.now - start
        #expect(result == .failed)
        #expect(elapsed < .seconds(2), "returned in \(elapsed)")
    }

    @Test func aFastCatchUpCancelsTheTimer() async {
        let start = ContinuousClock.now
        let result = await withTimeLimit(.seconds(30)) { 42 }
        guard case .finished(42) = result else {
            Issue.record("expected the value, got \(result)")
            return
        }
        #expect(ContinuousClock.now - start < .seconds(5))
    }

    @Test func aRealCatchUpAgainstAMockServer() async throws {
        // the engine's own catch-up, bounded: an empty server answers quickly
        let server = MockServer { r in
            switch r.path {
            case "/api/docs": return MockServer.Reply(chunks: [Data("[]".utf8)], headers: ["Taisce-Seq": "7"])
            case "/api/changes": return .json(#"{"seq":7,"changes":[],"more":false}"#)
            default: return MockServer.Reply(status: 404, chunks: [Data(#"{"error":"no"}"#.utf8)])
            }
        }
        let cache = try Cache.inMemory()
        let sync = SyncEngine(api: server.client(), cache: cache)
        let result = await BackgroundRefresh.run(limit: .seconds(10), catchUp: {
            let before = try await cache.lastSeq()
            try await sync.catchUp()
            return try await cache.lastSeq() != before
        }, reconcile: {})
        #expect(result == .newData, "the cursor moved from 0 to the head")
        #expect(try await cache.lastSeq() == 7)
    }
}
