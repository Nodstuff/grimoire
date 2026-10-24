import SwiftUI
import UIKit

/// Haptics, used sparingly and only where iOS itself would: a selection
/// change, a decision landing, a destructive act. No-ops on the Mac (and
/// on iPads without a Taptic Engine). In SwiftUI prefer
/// `.sensoryFeedback(…)` bound to the state that changed; this is for
/// imperative call sites (a button action, a UIKit delegate).
@MainActor
enum Haptics {
    /// a picker, chip or segment changed
    static func selection() {
        #if !targetEnvironment(macCatalyst)
        UISelectionFeedbackGenerator().selectionChanged()
        #endif
    }

    /// something physical: a drop landing, a sheet snapping
    static func impact(_ style: UIImpactFeedbackGenerator.FeedbackStyle = .light) {
        #if !targetEnvironment(macCatalyst)
        UIImpactFeedbackGenerator(style: style).impactOccurred()
        #endif
    }

    /// a decision landed: Keep, Apply, a to-do done
    static func success() { notify(.success) }

    /// a refusal, a parked edit
    static func warning() { notify(.warning) }

    static func error() { notify(.error) }

    private static func notify(_ type: UINotificationFeedbackGenerator.FeedbackType) {
        #if !targetEnvironment(macCatalyst)
        UINotificationFeedbackGenerator().notificationOccurred(type)
        #endif
    }
}
