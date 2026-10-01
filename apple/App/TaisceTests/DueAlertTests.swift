import Foundation
import Testing
import TaisceKit
import UserNotifications
@testable import Taisce

/// A notification center that keeps its pending requests in memory.
@MainActor
final class FakeDueAlertCenter: DueAlertCenter {
    var onAction: (@MainActor (DueAlertAction, String, String) async -> Void)?
    var status: DueAlertStatus = .allowed
    var requested = 0
    var categories = 0
    var pendingByID: [String: PlannedAlert] = [:]
    var added: [String] = []
    var removed: [String] = []

    func authorizationStatus() async -> DueAlertStatus { status }
    func requestAuthorization() async {
        requested += 1
        status = .allowed
    }
    func registerCategory() { categories += 1 }
    func pending() async -> [PlannedAlert] { Array(pendingByID.values) }
    func add(_ alert: PlannedAlert) async throws {
        added.append(alert.identifier)
        pendingByID[alert.identifier] = alert
    }
    func removePending(_ identifiers: [String]) {
        removed += identifiers
        for id in identifiers { pendingByID[id] = nil }
    }
}

@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct DueAlertTests {
    let dublin = TimeZone(identifier: "Europe/Dublin")!
    let now = Date(timeIntervalSince1970: 1_790_000_000)  // 2026-09-21T14:13:20Z

    func coordinator(_ center: FakeDueAlertCenter) -> NotificationCoordinator {
        let tz = dublin
        let now = now
        return NotificationCoordinator(center: center, now: { now }, timeZone: { tz })
    }

    func input(_ id: String, inHours h: Double, title: String = "t") -> DueAlertInput {
        DueAlertInput(day: "2026-09-21", itemID: id, title: title, docTitle: "To-do", kind: .timed(now.addingTimeInterval(h * 3600)))
    }

    @Test func reconcileAddsRemovesAndReplaces() async {
        let center = FakeDueAlertCenter()
        let c = coordinator(center)
        #expect(center.categories == 1)
        await c.reconcile(with: [input("a", inHours: 1), input("b", inHours: 2), input("c", inHours: 3)])
        #expect(Set(center.pendingByID.keys) == ["todo:2026-09-21/a", "todo:2026-09-21/b", "todo:2026-09-21/c"])
        // someone else's request is never touched
        center.pendingByID["other"] = PlannedAlert(identifier: "other", day: "", itemID: "", title: "", body: "", fireDate: now, components: DateComponents())
        center.added = []
        await c.reconcile(with: [input("a", inHours: 1), input("b", inHours: 5), input("d", inHours: 4)])
        #expect(Set(center.removed) == ["todo:2026-09-21/b", "todo:2026-09-21/c"])
        #expect(Set(center.added) == ["todo:2026-09-21/b", "todo:2026-09-21/d"])
        #expect(Set(center.pendingByID.keys) == ["other", "todo:2026-09-21/a", "todo:2026-09-21/b", "todo:2026-09-21/d"])
        // nothing changed: no churn
        center.added = []
        center.removed = []
        await c.reconcile(with: [input("a", inHours: 1), input("b", inHours: 5), input("d", inHours: 4)])
        #expect(center.added.isEmpty && center.removed.isEmpty)
    }

    @Test func deniedClearsOurAlerts() async {
        let center = FakeDueAlertCenter()
        let c = coordinator(center)
        await c.reconcile(with: [input("a", inHours: 1)])
        center.status = .denied
        await c.reconcile(with: [input("a", inHours: 1)])
        #expect(center.pendingByID.isEmpty)
        #expect(c.status == .denied)
        await c.requestAuthorization()
        #expect(center.requested == 1 && c.status == .allowed)
    }

    @Test func actionsQueueThroughTheOutboxAndHoldUntilSynced() async throws {
        let center = FakeDueAlertCenter()
        let c = coordinator(center)
        let cache = try Cache.inMemory()
        // no API: offline, the writes only queue
        c.connect(cache: cache, api: nil, sync: nil)
        let inputs = [input("a", inHours: 1), input("b", inHours: 1)]
        await c.reconcile(with: inputs)
        #expect(center.pendingByID.count == 2)

        await center.onAction?(.done, "2026-09-21", "a")
        await c.reconcile(with: inputs)  // the cache doesn't show it yet
        #expect(center.pendingByID["todo:2026-09-21/a"] == nil)

        await center.onAction?(.snooze, "2026-09-21", "b")
        let b = try #require(center.pendingByID["todo:2026-09-21/b"])
        // 14:13:20Z + 1 h, to the minute: 15:13Z, 16:13 IST
        // held locally at 15:13:00Z; the pending request (same minute) stays
        #expect(c.overrides["2026-09-21/b"] == .snoozed(Date(timeIntervalSince1970: 1_790_000_000 + 3600 - 20)))
        #expect(b.body == "Due 16:13 · To-do")

        await center.onAction?(.tomorrow, "2026-09-21", "b")
        #expect(center.pendingByID["todo:2026-09-21/b"]?.body == "Due 09:00 · To-do")

        let queued = try await cache.pendingOutbox()
        #expect(queued.map(\.path) == ["/api/todo/toggle", "/api/todo/deadline", "/api/todo/deadline"])
        let data = try #require(queued.last?.body)
        let last = try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        // 09:00 Dublin as a UTC instant (IST: 08:00Z), plus the local day for pre-UTC servers
        #expect(last["due_at"] as? String == "2026-09-22T08:00:00Z")
        #expect(last["deadline"] as? String == "2026-09-22" && last["item_id"] as? String == "b")

        // a sync shows a done and b's new time: the overrides drop
        var a = inputs[0]; a.done = true
        var bSynced = inputs[1]
        bSynced.kind = try #require(DueAlertInput(day: "2026-09-21", itemID: "b", title: "t", docTitle: "To-do", deadline: "2026-09-22", dueTime: "09:00", timeZone: dublin)).kind
        await c.reconcile(with: [a, bSynced])
        #expect(c.overrides.isEmpty)
    }

    /// Snoozing at 01:10 IST on 2026-10-25 (00:10Z): an hour later is
    /// 01:10 GMT (01:10Z), the second 01:10 of the night.
    @Test func snoozeInTheRepeatedHourKeepsTheInstant() async throws {
        let center = FakeDueAlertCenter()
        let tz = dublin
        let start = Date(timeIntervalSince1970: 1_792_887_000)  // 2026-10-25T00:10:00Z
        let c = NotificationCoordinator(center: center, now: { start }, timeZone: { tz })
        c.connect(cache: try Cache.inMemory(), api: nil, sync: nil)
        let item = DueAlertInput(day: "2026-10-24", itemID: "a", title: "t", docTitle: "To-do", kind: .timed(start.addingTimeInterval(600)))
        await c.reconcile(with: [item])
        await center.onAction?(.snooze, "2026-10-24", "a")
        #expect(c.overrides["2026-10-24/a"] == .snoozed(start.addingTimeInterval(3600)))
        #expect(center.pendingByID["todo:2026-10-24/a"]?.fireDate == start.addingTimeInterval(3600))
        #expect(center.pendingByID["todo:2026-10-24/a"]?.body == "Due 01:10 · To-do")
    }

    @Test func requestsRoundTripThroughTheSystemShape() throws {
        let alert = try #require(DueAlertPlanner.plan([input("a", inHours: 30, title: "Ship it")], timeZone: dublin, now: now).first)
        let request = SystemDueAlertCenter.request(for: alert)
        #expect(request.content.categoryIdentifier == "TODO_DUE")
        let back = try #require(SystemDueAlertCenter.plannedAlert(from: request))
        #expect(back.signature == alert.signature)
        #expect(back.day == "2026-09-21" && back.itemID == "a")
        let foreign = UNNotificationRequest(identifier: "x", content: UNMutableNotificationContent(), trigger: nil)
        #expect(SystemDueAlertCenter.plannedAlert(from: foreign) == nil)
    }

    @Test func releaseRefusesPlainHTTP() {
        #expect(ServerURLPolicy.accepts("https://taisce.null.ie"))
        #expect(ServerURLPolicy.check("http://taisce.null.ie", allowLoopbackHTTP: true) == .failure(.insecure))
        #expect(ServerURLPolicy.check("http://127.0.0.1:7425", allowLoopbackHTTP: false) == .failure(.insecure))
        #expect(ServerURLPolicy.check("http://localhost:7425", allowLoopbackHTTP: false) == .failure(.insecure))
        #expect((try? ServerURLPolicy.check("http://127.0.0.1:7425", allowLoopbackHTTP: true).get()) != nil)
        #expect(ServerURLPolicy.check("ftp://x", allowLoopbackHTTP: true) == .failure(.notAURL))
        #expect(ServerURLPolicy.check("taisce.null.ie", allowLoopbackHTTP: true) == .failure(.notAURL))
        // this is a Debug build: loopback http is allowed
        #expect(ServerURLPolicy.allowsLoopbackHTTP)
    }

    /// The simulator reports this class for every file whatever was set
    /// (a file created as `.none` reads back the same, and a directory
    /// reads nil), so this pins the readback and that setting it doesn't
    /// throw; only a device tells the classes apart.
    @Test func cacheFilesAreProtectedUntilFirstUnlock() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "protect-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let path = dir.appending(path: "cache.sqlite").path(percentEncoded: false)
        func protection(_ p: String) throws -> URLFileProtection? {
            try URL(filePath: p).resourceValues(forKeys: [.fileProtectionKey]).fileProtection
        }
        let cache = try Cache(path: path)
        try await cache.setLastSeq(1)
        for file in [path, path + "-wal", path + "-shm"] {
            #expect(try protection(file) == .completeUntilFirstUserAuthentication, "\(file)")
        }
    }
}
