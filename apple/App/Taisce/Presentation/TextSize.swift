import SwiftUI

/// The Mac's doc text size (View › Bigger Text / Smaller Text / Actual
/// Size, ⌘+ ⌘- ⌘0). A step on the Dynamic Type scale, applied to the doc
/// screen only, so reading and editing grow together and the sidebar keeps
/// its size. The iPhone follows the system's Text Size instead.
enum DocTextSize {
    static let key = "doc.textSize"
    static let steps: [DynamicTypeSize] = [.medium, .large, .xLarge, .xxLarge, .xxxLarge, .accessibility1, .accessibility2]
    /// Catalyst's default, `.large`
    static let actual = 1

    static func size(_ step: Int) -> DynamicTypeSize { steps[clamp(step)] }
    static func bigger(_ step: Int) -> Int { clamp(step + 1) }
    static func smaller(_ step: Int) -> Int { clamp(step - 1) }
    static func canGrow(_ step: Int) -> Bool { clamp(step) < steps.count - 1 }
    static func canShrink(_ step: Int) -> Bool { clamp(step) > 0 }

    private static func clamp(_ step: Int) -> Int { min(max(step, 0), steps.count - 1) }
}
