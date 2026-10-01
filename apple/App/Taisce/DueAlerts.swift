import Foundation
import Observation
import UserNotifications

enum DueAlertStatus: Hashable, Sendable {
    case notDetermined, allowed, denied
}

/// Whether due alerts may be shown, and asking for it. The UI only talks to
/// this protocol; TaisceKit's NotificationCoordinator (on ios-client) slots
/// in by conforming, replacing `SystemDueAlerts`.
@MainActor
protocol DueAlertPermission: AnyObject {
    var status: DueAlertStatus { get }
    func refresh() async
    func requestAuthorization() async
}

/// Notification permission straight from `UNUserNotificationCenter`.
@MainActor @Observable
final class SystemDueAlerts: DueAlertPermission {
    private(set) var status: DueAlertStatus = .allowed

    func refresh() async {
        let settings = await UNUserNotificationCenter.current().notificationSettings()
        status = switch settings.authorizationStatus {
        case .notDetermined: .notDetermined
        case .denied: .denied
        default: .allowed
        }
    }

    func requestAuthorization() async {
        _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
        await refresh()
    }
}

#if DEBUG
/// Previews: a fixed status, and asking flips it to allowed.
@MainActor @Observable
final class PreviewDueAlerts: DueAlertPermission {
    private(set) var status: DueAlertStatus

    init(_ status: DueAlertStatus) {
        self.status = status
    }

    func refresh() async {}
    func requestAuthorization() async { status = .allowed }
}
#endif
