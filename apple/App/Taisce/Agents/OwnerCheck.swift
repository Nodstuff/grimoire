import Foundation
import LocalAuthentication
import TaisceKit

/// "Allow once with Face ID": the person, not just the unlocked phone,
/// says yes to an approval.
protocol OwnerCheck: Sendable {
    /// True when the device's owner confirmed; false when they cancelled.
    func confirm(_ reason: String) async -> Bool
}

/// Face ID, Touch ID or the passcode.
struct DeviceOwnerCheck: OwnerCheck {
    func confirm(_ reason: String) async -> Bool {
        let context = LAContext()
        context.localizedCancelTitle = "Cancel"
        var error: NSError?
        guard context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &error) else { return false }
        return (try? await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)) ?? false
    }

    /// What the button says: the biometry this device has.
    static var buttonWord: String {
        let context = LAContext()
        _ = context.canEvaluatePolicy(.deviceOwnerAuthenticationWithBiometrics, error: nil)
        switch context.biometryType {
        case .faceID: return "Face ID"
        case .touchID: return "Touch ID"
        case .opticID: return "Optic ID"
        default: return "your passcode"
        }
    }
}

#if DEBUG
/// UI runs on a simulator with no enrolled face: `-fabricOwnerCheck pass`
/// (Debug builds only) stands in for the person.
struct PassingOwnerCheck: OwnerCheck {
    func confirm(_ reason: String) async -> Bool { true }
}
#endif
