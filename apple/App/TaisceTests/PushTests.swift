import Foundation
import Testing
import TaisceKit
import UIKit
@testable import Taisce

/// APNs glue in the app: build settings, the silent push's payload, and the
/// background run cancelling a stale alert within its limit.
@MainActor @Suite(.serialized, .timeLimit(.minutes(1))) struct PushTests {
    let now = Date(timeIntervalSince1970: 1_790_000_000)

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
        UserDefaults.standard.set("http://127.0.0.1:19", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel()
        // a background launch: no boot yet, the push connects on its own
        let start = ContinuousClock.now
        let result = await m.handleSilentPush(seq: 5)
        #expect(m.api != nil && m.authPhase == .notRequired)
        #expect(result == .failed, "nothing listens on port 19")
        #expect(ContinuousClock.now - start < .seconds(25))
        await m.stopSync()
    }

    @Test func aLocalDaemonRegistersNoToken() async {
        UserDefaults.standard.set("http://127.0.0.1:19", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel()
        await m.boot()
        await m.stopSync()
        // no bearer, no registry: a token waits
        #expect(await m.push.didRegister(token: "aa") == .waiting)
    }

    @Test func bootReturnsQuicklyWhenRegistrationNeverCallsBack() async {
        UserDefaults.standard.set("http://127.0.0.1:29", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel()
        m.discover = { _ in nil }
        // iOS never calls didRegister/didFail in a test host
        let start = ContinuousClock.now
        await m.boot()
        #expect(ContinuousClock.now - start < .seconds(2))
        await m.stopSync()
        // signed in, with a token and a server that never answers: still returns at once
        await m.push.didRegister(token: String(repeating: "ab", count: 32))
        var asked = false
        let t0 = ContinuousClock.now
        m.beginPushRegistration(server: "https://hang.invalid", registry: HangingRegistry()) { asked = true }
        #expect(asked, "the token is requested")
        #expect(ContinuousClock.now - t0 < .milliseconds(500))
        // and sign-out drops the hung send, bounded
        let t1 = ContinuousClock.now
        await m.pushSessionEnding()
        #expect(ContinuousClock.now - t1 < PushConfig.unregisterLimit + .seconds(1))
        #expect(m.pushTask == nil)
    }
}

/// A server that never answers (cancellable).
struct HangingRegistry: DeviceRegistry {
    func registerDevice(_ registration: DeviceRegistration) async throws {
        try await Task.sleep(for: .seconds(3600))
    }
    func unregisterDevice(token: String) async throws {
        try await Task.sleep(for: .seconds(3600))
    }
}

@Suite struct PushEnvironmentFromProfileTests {
    func profile(_ aps: String?, key: String = "aps-environment") -> Data {
        let ents = aps.map { "<key>\(key)</key><string>\($0)</string>" } ?? ""
        let xml = "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>Entitlements</key><dict>\(ents)</dict></dict></plist>"
        // a provisioning profile is CMS-wrapped: binary bytes around the plist
        return Data([0x30, 0x82, 0x01, 0x00]) + Data(xml.utf8) + Data([0xA0, 0x00])
    }
    @Test func developmentProfileMeansSandbox() {
        #expect(PushConfig.environment(profile: profile("development")) == .sandbox)
    }
    @Test func productionProfileMeansProduction() {
        #expect(PushConfig.environment(profile: profile("production")) == .production)
    }
    @Test func noProfileMeansProduction() {
        #expect(PushConfig.environment(profile: nil) == .production)
        #expect(PushConfig.environment(profile: profile(nil)) == .production)
    }
    @Test func macProfileUsesTheDeveloperKey() {
        let key = "com.apple.developer.aps-environment"
        #expect(PushConfig.environment(profile: profile("development", key: key)) == .sandbox)
        #expect(PushConfig.environment(profile: profile("production", key: key)) == .production)
    }
    @Test func profileLivesWhereEachPlatformPutsIt() {
        let app = URL(filePath: "/x/Taisce.app")
        #expect(PushConfig.profileURL(bundle: app, mac: false).path(percentEncoded: false) == "/x/Taisce.app/embedded.mobileprovision")
        #expect(PushConfig.profileURL(bundle: app, mac: true).path(percentEncoded: false) == "/x/Taisce.app/Contents/embedded.provisionprofile")
        #if targetEnvironment(macCatalyst)
        #expect(PushConfig.isMac)
        #else
        #expect(!PushConfig.isMac)
        #endif
    }
    /// The installed build's own profile: Xcode signs every test host with
    /// a development one.
    @Test func thisBuildsProfileIsFound() throws {
        let url = PushConfig.profileURL(bundle: Bundle.main.bundleURL, mac: PushConfig.isMac)
        guard FileManager.default.fileExists(atPath: url.path(percentEncoded: false)) else { return } // simulator: unsigned
        #expect(PushConfig.environment == .sandbox)
    }
}
