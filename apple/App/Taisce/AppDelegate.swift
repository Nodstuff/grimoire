import Foundation
import TaisceKit
import UIKit

/// UIKit's half of APNs. It owns the app model, so a silent push that
/// launches the app in the background finds one before any scene exists.
@MainActor
final class AppDelegate: NSObject, UIApplicationDelegate {
    let model: AppModel

    override init() {
        // the Mac's first unsandboxed launch: the old container's cache and
        // preferences come over before the model reads either
        let report = AppPaths.migrateSandboxContainer()
        #if targetEnvironment(macCatalyst)
        // runs a crash or force quit left behind
        if !AppPaths.isTestHost { CodeRun.sweepStaleRuns() }
        #endif
        model = AppModel(migrationBlocked: SandboxMigration.blockingReason(report))
        super.init()
    }
    /// why iOS gave no token (no network, no entitlement in a dev build)
    private(set) var registrationError: String?

    func application(_ application: UIApplication, didRegisterForRemoteNotificationsWithDeviceToken deviceToken: Data) {
        registrationError = nil
        let push = model.push
        // bounded: a hung POST must not keep a task alive forever
        Task { _ = await withTimeLimit(PushConfig.registerLimit) { await push.didRegister(deviceToken: deviceToken) } }
    }

    func application(_ application: UIApplication, didFailToRegisterForRemoteNotificationsWithError error: any Error) {
        // local alerts carry on; the next foreground start asks again
        registrationError = error.localizedDescription
    }

    /// `application(_:didReceiveRemoteNotification:fetchCompletionHandler:)`,
    /// async: the returned result is the completion handler's.
    func application(_ application: UIApplication, didReceiveRemoteNotification userInfo: [AnyHashable: Any]) async -> UIBackgroundFetchResult {
        UIBackgroundFetchResult(await model.handleSilentPush(seq: SilentPush.seq(from: userInfo)))
    }
}
