import Foundation
import SwiftUI
import Testing
import TaisceKit
@testable import Taisce

/// Review 2026-11-22 #12: controls drawn under 44 pt take a finger's tap.
@Suite struct TapAreaTests {
    func reach(_ a: TapArea) -> CGSize {
        CGSize(width: a.drawn.width + a.insets.leading + a.insets.trailing, height: a.drawn.height + 2 * a.insets.v)
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
        #expect(TapArea.checkbox.insets.trailing <= TapArea.checkboxGap)
    }

    /// Review of cc02935 (nit): a nested checklist's boxes start one gap
    /// right of their parent's and sit close enough below that the two
    /// reaches met (~6 x 8 pt). The parent's reach now stops where the
    /// nested box's starts; a plain item keeps the full 44.
    @Test func aParentCheckboxNeverReachesItsNestedChild() throws {
        func items(_ md: String) -> [RenderNode.ListItem] {
            guard case .list(_, _, let items) = BlockRenderer.render(markdown: md).first else { return [] }
            return items
        }
        let list = items("- [ ] parent\n  - [x] nested\n- [ ] over bullets\n  - plain")
        try #require(list.count == 2)
        guard case .list(_, _, let kids) = list[0].children.last else { Issue.record("no nested list"); return }
        let parent = TapArea.checkbox(for: list[0])
        let child = TapArea.checkbox(for: try #require(kids.first))
        let bullets = list[1]
        #expect(parent == .checkboxOverNested && child == .checkbox)
        #expect(TapArea.checkbox(for: bullets) == .checkbox, "a nested bullet list takes no taps")
        // x from the parent box's leading edge: the child's box starts at 28 + gap
        let parentRight = parent.drawn.width + parent.insets.trailing
        let childLeft = parent.drawn.width + TapArea.checkboxGap - child.insets.leading
        #expect(parentRight <= childLeft, "overlap \(parentRight - childLeft) pt")
        #expect(parent.insets.leading == TapArea.checkbox.insets.leading, "it still reaches out to the leading side")
        #expect(reach(parent).width > parent.drawn.width)
    }

    @Test func aControlAlreadyBigEnoughIsLeftAlone() {
        let big = TapArea(drawn: CGSize(width: 50, height: 60), target: CGSize(width: 44, height: 44))
        #expect(big.insets.leading == 0 && big.insets.trailing == 0 && big.insets.v == 0)
    }

    /// Nothing drawn moves: the modifier's layout size is the drawn size.
    @Test @MainActor func theLayoutKeepsTheDrawnSize() {
        let host = UIHostingController(rootView: Color.red.frame(width: 28, height: 28).tapArea(.checkbox))
        #expect(host.sizeThatFits(in: CGSize(width: 500, height: 500)) == CGSize(width: 28, height: 28))
        let edit = UIHostingController(rootView: Color.red.frame(width: 26, height: 26).tapArea(.editSource))
        #expect(edit.sizeThatFits(in: CGSize(width: 500, height: 500)) == CGSize(width: 26, height: 26))
    }
}
