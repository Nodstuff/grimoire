import Foundation
import SwiftUI
import Testing
@testable import Taisce

/// Review 2026-11-22 #12: controls drawn under 44 pt take a finger's tap.
@Suite struct TapAreaTests {
    func reach(_ a: TapArea) -> CGSize {
        CGSize(width: a.drawn.width + 2 * a.insets.h, height: a.drawn.height + 2 * a.insets.v)
    }

    @Test func eachSmallControlReachesAFingersWidth() {
        #expect(reach(.editSource) == CGSize(width: 44, height: 44))
        #expect(reach(.newFolder) == CGSize(width: 44, height: 44))
        #expect(reach(.checkbox).width == 44)
    }

    /// List checkboxes sit 8 pt apart: the reach stops at the row pitch, so
    /// a tap between two rows never ticks the wrong one.
    @Test func checkboxesNeverReachTheirNeighbours() {
        let pitch = TapArea.checkbox.drawn.height + 8
        #expect(reach(.checkbox).height == pitch)
        // and sideways it stays inside the 10 pt gap before the item's text
        #expect(TapArea.checkbox.insets.h <= 10)
    }

    @Test func aControlAlreadyBigEnoughIsLeftAlone() {
        let big = TapArea(drawn: CGSize(width: 50, height: 60), target: CGSize(width: 44, height: 44))
        #expect(big.insets.h == 0 && big.insets.v == 0)
    }

    /// Nothing drawn moves: the modifier's layout size is the drawn size.
    @Test @MainActor func theLayoutKeepsTheDrawnSize() {
        let host = UIHostingController(rootView: Color.red.frame(width: 28, height: 28).tapArea(.checkbox))
        #expect(host.sizeThatFits(in: CGSize(width: 500, height: 500)) == CGSize(width: 28, height: 28))
        let edit = UIHostingController(rootView: Color.red.frame(width: 26, height: 26).tapArea(.editSource))
        #expect(edit.sizeThatFits(in: CGSize(width: 500, height: 500)) == CGSize(width: 26, height: 26))
    }
}
