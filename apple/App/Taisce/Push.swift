import Foundation
import TaisceKit
import UIKit
import UserNotifications

/// This build's APNs settings.
enum PushConfig {
    /// The APNs gateway this install's token belongs to, read from the
    /// provisioning profile the app was signed with — not the build
    /// configuration: a Release build installed from Xcode carries a
    /// development profile (`aps-environment` development → a sandbox token).
    /// No embedded profile (App Store) means production.
    static var environment: PushEnvironment {
        environment(profile: (try? Data(contentsOf: profileURL(bundle: Bundle.main.bundleURL, mac: isMac))))
    }

    static var isMac: Bool {
        #if targetEnvironment(macCatalyst)
        true
        #else
        false
        #endif
    }

    /// Where the signing profile sits: `embedded.mobileprovision` at the top
    /// of an iOS bundle, `Contents/embedded.provisionprofile` in a Mac one.
    static func profileURL(bundle: URL, mac: Bool) -> URL {
        mac ? bundle.appending(path: "Contents/embedded.provisionprofile")
            : bundle.appending(path: "embedded.mobileprovision")
    }

    /// The entitlement is `aps-environment` in an iOS profile and
    /// `com.apple.developer.aps-environment` in a Mac (Catalyst) one.
    static func environment(profile: Data?) -> PushEnvironment {
        guard let profile,
              let text = String(data: profile, encoding: .isoLatin1),
              let start = text.range(of: "<?xml"),
              let end = text.range(of: "</plist>", range: start.lowerBound..<text.endIndex),
              let xml = String(text[start.lowerBound..<end.upperBound]).data(using: .isoLatin1),
              let plist = try? PropertyListSerialization.propertyList(from: xml, format: nil) as? [String: Any],
              let ents = plist["Entitlements"] as? [String: Any],
              let aps = (ents["aps-environment"] ?? ents["com.apple.developer.aps-environment"]) as? String
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

/// The server's alert push for a comment on one of your share links:
/// `{"aps":{"alert":…},"kind":"share_comment","doc_id":…,"share_id":…}`.
/// A tap opens that doc's link comments.
enum ShareCommentPush {
    static let kind = "share_comment"

    static func docID(from userInfo: [AnyHashable: Any]) -> DocID? {
        guard userInfo["kind"] as? String == kind, let doc = userInfo["doc_id"] as? String, !doc.isEmpty else { return nil }
        return doc
    }

    /// The app's model (the delegate owns it): taps become its request,
    /// which the frontmost window takes (`RootView`); no window holds a
    /// closure here, so a closed window can't swallow one.
    @MainActor static weak var model: AppModel?

    /// A tap on the notification.
    @MainActor static func deliver(_ doc: DocID) {
        model?.requestLinkComments(doc)
    }

    /// One arrived while the app is open: refresh the badges.
    @MainActor static func arrived(_ doc: DocID) {
        guard let model else { return }
        Task { await model.shareLinks.commentArrived(on: doc) }
    }
}

/// The server's alert push when one of your Claude sessions comes to need
/// you, stalls or finishes a turn (Agent Fabric):
/// `{"aps":{"alert":{"title":<address>,"body":"Needs you"},"thread-id":"fabric:<session>"},
/// "kind":"fabric_session","session_id":…,"runtime_id":…,"state":"needs_you"}`.
/// The words are the server's; a tap opens Agents with that session in view.
enum FabricSessionPush {
    static let kind = "fabric_session"

    static func session(from userInfo: [AnyHashable: Any]) -> FleetSessionKey? {
        guard userInfo["kind"] as? String == kind,
              let session = userInfo["session_id"] as? String, !session.isEmpty,
              let runtime = userInfo["runtime_id"] as? String, !runtime.isEmpty
        else { return nil }
        return FleetSessionKey(runtimeID: runtime.lowercased(), id: session.lowercased())
    }

    /// The app's model (the delegate owns it), as `ShareCommentPush.model`.
    @MainActor static weak var model: AppModel?

    /// A tap on the notification: the frontmost window takes the request (`RootView`).
    @MainActor static func deliver(_ session: FleetSessionKey) {
        model?.fleet.requestSession(session)
    }

    /// How one arriving while the app is open is shown: not at all when the
    /// Agents screen is in front (the row itself has just changed under the
    /// person's eyes), like any other notification otherwise.
    @MainActor static func presentation(agentsOnScreen: Bool) -> UNNotificationPresentationOptions {
        agentsOnScreen ? [] : [.banner, .list, .sound]
    }

    @MainActor static func presentation() -> UNNotificationPresentationOptions {
        presentation(agentsOnScreen: model?.fleet.isOnScreen ?? false)
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
        settleOwedPushDeleteElsewhere()
        guard authPhase == .signedIn, let api else {
            enqueuePush { await $0.disconnect() }
            return
        }
        beginPushRegistration(server: serverURL, registry: api) {
            UIApplication.shared.registerForRemoteNotifications()
        }
    }

    /// A device delete owed to another server (a sign-out or a switch away
    /// from it while offline): sent now if this device still holds a
    /// bearer for that server, else kept for the next sign-in there. One
    /// owed to the current server goes ahead of its registration
    /// (`PushRegistrar.connect`). Queued like the rest, never awaited.
    func settleOwedPushDeleteElsewhere() {
        let current = serverURL
        let registryFor = pushRegistry
        enqueuePush { push in
            guard let owed = await push.owedDeleteServer, owed != current, let registry = await registryFor(owed) else { return }
            _ = await withTimeLimit(PushConfig.registerLimit) { await push.settleOwedDelete(server: owed, registry: registry) }
        }
    }

    /// A registry for `server` from the tokens this device keeps for it
    /// (the Keychain holds them per server), or nil when it has none.
    static func storedRegistry(_ server: String) async -> (any DeviceRegistry)? {
        guard let url = URL(string: server), let store = try? KeychainTokenStore() else { return nil }
        let auth = AuthSession(oauth: OAuthClient(baseURL: url, appLinkHosts: AppLinks.hosts()), store: store, shield: backgroundTask)
        guard await auth.state == .signedIn else { return nil }
        return APIClient(config: ServerConfig(baseURL: url, tokenProvider: auth))
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
