import Foundation
import TaisceKit
import UIKit

/// This build's APNs settings.
enum PushConfig {
    /// The APNs gateway this install's token belongs to, read from the
    /// provisioning profile the app was signed with — not the build
    /// configuration: a Release build installed from Xcode carries a
    /// development profile (`aps-environment` development → a sandbox token).
    /// No embedded profile (App Store) means production.
    static var environment: PushEnvironment {
        let url = Bundle.main.url(forResource: "embedded", withExtension: "mobileprovision")
        return environment(profile: url.flatMap { try? Data(contentsOf: $0) })
    }

    static func environment(profile: Data?) -> PushEnvironment {
        guard let profile,
              let text = String(data: profile, encoding: .isoLatin1),
              let start = text.range(of: "<?xml"),
              let end = text.range(of: "</plist>", range: start.lowerBound..<text.endIndex),
              let xml = String(text[start.lowerBound..<end.upperBound]).data(using: .isoLatin1),
              let plist = try? PropertyListSerialization.propertyList(from: xml, format: nil) as? [String: Any],
              let ents = plist["Entitlements"] as? [String: Any],
              let aps = ents["aps-environment"] as? String
        else { return .production }
        return aps == "development" ? .sandbox : .production
    }

    /// `CFBundleShortVersionString (CFBundleVersion)`, e.g. `0.1.0 (1)`.
    static func appVersion(info: [String: Any]? = Bundle.main.infoDictionary) -> String {
        let short = info?["CFBundleShortVersionString"] as? String ?? "0"
        let build = info?["CFBundleVersion"] as? String ?? "0"
        return "\(short) (\(build))"
    }

    /// How long sign-out (or a server switch) waits for the device delete.
    static let unregisterLimit: Duration = .seconds(5)
    /// How long one registration send may run (it is never awaited by launch).
    static let registerLimit: Duration = .seconds(30)
}

/// The server's silent push: `{"aps":{"content-available":1},"seq":N}`.
enum SilentPush {
    /// The change-log head the push announces, if it carries one.
    static func seq(from userInfo: [AnyHashable: Any]) -> Int? {
        switch userInfo["seq"] {
        case let n as Int: n
        case let n as NSNumber: n.intValue
        case let s as String: Int(s)
        default: nil
        }
    }
}

extension UIBackgroundFetchResult {
    init(_ result: BackgroundRefreshResult) {
        switch result {
        case .newData: self = .newData
        case .noData: self = .noData
        case .failed: self = .failed
        }
    }
}

extension AppModel {
    /// After sign-in (and on every foreground start): point the registrar
    /// at this server and ask iOS for the token, which iOS hands back
    /// through `AppDelegate` whenever it likes (or never: no network, a test
    /// host); the registrar sends it when it changed or is a day old. LOCAL
    /// mode (no bearer) registers nothing. Never awaited: launch must not
    /// wait on APNs or on the device POST.
    func pushSessionStarted() {
        guard authPhase == .signedIn, let api else {
            enqueuePush { await $0.disconnect() }
            return
        }
        beginPushRegistration(server: serverURL, registry: api) {
            UIApplication.shared.registerForRemoteNotifications()
        }
    }

    /// Queue the registrar's connect (bounded by `registerLimit`) and ask
    /// for a token; returns at once.
    func beginPushRegistration(server: String, registry: any DeviceRegistry, requestToken: () -> Void) {
        enqueuePush { push in
            _ = await withTimeLimit(PushConfig.registerLimit) { await push.connect(server: server, registry: registry) }
        }
        requestToken()
    }

    /// Registrar work runs in order, off the caller.
    private func enqueuePush(_ op: @escaping @Sendable (PushRegistrar) async -> Void) {
        let (previous, push) = (pushTask, push)
        pushTask = Task {
            await previous?.value
            await op(push)
        }
    }

    /// Sign-out or a server switch: drop any send still out, then delete
    /// this device on the server while the bearer still works, bounded so
    /// an offline sign-out isn't held up.
    func pushSessionEnding() async {
        pushTask?.cancel()
        pushTask = nil
        let push = push
        _ = await withTimeLimit(PushConfig.unregisterLimit) { await push.unregister() }
    }

    /// A silent push: one bounded catch-up, then the due alerts follow the
    /// cache (a to-do done or moved elsewhere cancels its alert here).
    func handleSilentPush(seq: Int?) async -> BackgroundRefreshResult {
        await connectIfNeeded()
        guard authPhase == .signedIn || authPhase == .notRequired, let sync, let cache else { return .noData }
        let dueAlerts = dueAlerts
        // the foreground stream already has it, or the cache is past it
        let cursor = (try? await cache.lastSeq()) ?? 0
        if syncStatus == .live || (seq.map { $0 <= cursor } ?? false) {
            _ = await withTimeLimit(BackgroundRefresh.reconcileLimit) { await dueAlerts.reconcile() }
            return .noData
        }
        return await BackgroundRefresh.run(
            catchUp: {
                let before = try await cache.lastSeq()
                try await sync.catchUp()
                return try await cache.lastSeq() != before
            },
            reconcile: { await dueAlerts.reconcile() }
        )
    }
}
