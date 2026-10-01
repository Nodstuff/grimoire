import Foundation
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// APNs glue in the app: build settings, the silent push's payload, and the
/// background run cancelling a stale alert within its limit.
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct PushTests {
    let now = Date(timeIntervalSince1970: 1_790_000_000)

    @Test func debugBuildsRegisterSandboxTokens() {
        // these tests only run in Debug; Release is `.production` by #if
        #expect(PushConfig.environment == .sandbox)
    }

    @Test func appVersionIsShortVersionAndBuild() {
        #expect(PushConfig.appVersion(info: ["CFBundleShortVersionString": "0.1.0", "CFBundleVersion": "7"]) == "0.1.0 (7)")
        #expect(PushConfig.appVersion(info: nil) == "0 (0)")
        #expect(PushConfig.appVersion().hasSuffix(")"))
    }

    @Test func silentPushSeq() {
        #expect(SilentPush.seq(from: ["aps": ["content-available": 1], "seq": 42]) == 42)
        #expect(SilentPush.seq(from: ["seq": NSNumber(value: 7)]) == 7)
        #expect(SilentPush.seq(from: ["seq": "9"]) == 9)
        #expect(SilentPush.seq(from: ["aps": ["content-available": 1]]) == nil)
    }

    @Test func fetchResultMapping() {
        #expect(UIBackgroundFetchResult(.newData) == .newData)
        #expect(UIBackgroundFetchResult(.noData) == .noData)
        #expect(UIBackgroundFetchResult(.failed) == .failed)
    }

    @Test func aHangingCatchUpStillCancelsTheStaleAlertInTime() async {
        let center = FakeDueAlertCenter()
        let tz = TimeZone(identifier: "Europe/Dublin") ?? .gmt
        let now = now
        let c = NotificationCoordinator(center: center, now: { now }, timeZone: { tz })
        let item = DueAlertInput(day: "2026-09-21", itemID: "a", title: "t", docTitle: "To-do", kind: .timed(now.addingTimeInterval(3600)))
        await c.reconcile(with: [item])
        #expect(center.pendingByID.keys.contains("todo:2026-09-21/a"))
        let start = ContinuousClock.now
        // the item was done elsewhere (the cache no longer has it); the
        // catch-up hangs far past the limit
        let result = await BackgroundRefresh.run(limit: .milliseconds(200), catchUp: {
            try await Task.sleep(for: .seconds(30))
            return true
        }, reconcile: { await c.reconcile(with: []) })
        #expect(result == .failed)
        #expect(ContinuousClock.now - start < .seconds(5))
        #expect(center.pendingByID.isEmpty, "the stale alert is gone")
    }

    @Test func aPushToAnUnreachableLocalDaemonFails() async {
        UserDefaults.standard.set("http://127.0.0.1:9", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel()
        // a background launch: no boot yet, the push connects on its own
        let start = ContinuousClock.now
        let result = await m.handleSilentPush(seq: 5)
        #expect(m.api != nil && m.authPhase == .notRequired)
        #expect(result == .failed, "nothing listens on port 9")
        #expect(ContinuousClock.now - start < .seconds(25))
        await m.stopSync()
    }

    @Test func aLocalDaemonRegistersNoToken() async {
        UserDefaults.standard.set("http://127.0.0.1:9", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel()
        await m.boot()
        await m.stopSync()
        // no bearer, no registry: a token waits
        #expect(await m.push.didRegister(token: "aa") == .waiting)
    }
}
