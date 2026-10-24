import Foundation
import SwiftUI
import Testing
import TaisceKit
@testable import Taisce

private let work = Workspace(id: "w1", name: "Work", sortKey: "b")
private let home = Workspace(id: "w2", name: "Home", sortKey: "a")
private let picker = WorkspacePicker(workspaces: [work, home], unsortedCount: 2, stored: .id("w1"))

/// The regular / compact split and the menu bar's routing.
@MainActor @Suite struct CommandTests {
    @Test func regularWidthIsTheSplitViewCompactKeepsTheTabs() {
        #expect(RootLayout.make(horizontal: .regular) == .split)
        #expect(RootLayout.make(horizontal: .compact) == .tabs)
        #expect(RootLayout.make(horizontal: nil) == .tabs)
    }

    @Test func workspaceShortcutsFollowTheSwitchersOrder() {
        // Home sorts first, Unsorted (it has docs) last
        #expect(picker.shortcut(1) == .id("w2"))
        #expect(picker.shortcut(2) == .id("w1"))
        #expect(picker.shortcut(3) == .unsorted)
        #expect(picker.shortcut(4) == nil)
        #expect(picker.shortcut(0) == nil)
        var off = picker
        off.enabled = false
        #expect(off.shortcut(1) == nil)
    }

    @Test func docTextSizeStepsAlongDynamicTypeAndStopsAtTheEnds() {
        #expect(DocTextSize.size(DocTextSize.actual) == .large)
        #expect(DocTextSize.size(DocTextSize.bigger(DocTextSize.actual)) == .xLarge)
        #expect(DocTextSize.smaller(0) == 0 && !DocTextSize.canShrink(0))
        let top = DocTextSize.steps.count - 1
        #expect(DocTextSize.bigger(top) == top && !DocTextSize.canGrow(top))
        // a stored value from a longer scale never crashes
        #expect(DocTextSize.size(99) == DocTextSize.steps.last && DocTextSize.size(-3) == .medium)
        // read mode scales by hand (Catalyst ignores dynamicTypeSize): 1 at actual size, growing per step
        #expect(DocTextSize.scale(DocTextSize.actual) == 1)
        #expect(DocTextSize.scale(DocTextSize.actual + 1) > 1 && DocTextSize.scale(DocTextSize.actual + 2) > DocTextSize.scale(DocTextSize.actual + 1))
        #expect(DocTextSize.scale(0) < 1)
    }

    @Test func todayIsClickableAgainAfterOpeningADocFromIt() {
        let r = Router()
        r.isPad = true
        #expect(r.sidebarSelection == .today)
        r.open(.doc("d1"))
        // the sidebar follows the doc in front, so a click on Today is a change
        #expect(r.sidebarSelection == .doc("d1"))
        r.select(.today)
        #expect(r.sidebarSelection == .today && r.padPath.isEmpty)
        r.open(.todos)
        #expect(r.sidebarSelection == .todos)
    }

    @Test func openActionsSidebarAndBooksCommands() {
        let r = Router()
        r.isPad = true
        // ⌘O: Search with docs, the field focused
        _ = r.perform(.open, picker: picker)
        #expect(r.padItem == .search && r.searchScopeRequest == .docs)
        // ⌘K: the palette
        _ = r.perform(.actions, picker: picker)
        #expect(r.showActions)
        // ⌘\: the sidebar goes and comes back (split view only)
        _ = r.perform(.toggleSidebar, picker: picker)
        #expect(r.sidebarHidden)
        _ = r.perform(.toggleSidebar, picker: picker)
        #expect(!r.sidebarHidden)
        #expect(!Router().canPerform(.toggleSidebar, signedIn: true, picker: picker), "a phone has no sidebar")
        // ⌥⌘N: a child of the doc in front, only with one
        #expect(!r.canPerform(.newDocHere, signedIn: true, picker: picker))
        r.select(.doc("d1"))
        #expect(r.canPerform(.newDocHere, signedIn: true, picker: picker))
        _ = r.perform(.newDocHere, picker: picker)
        #expect(r.showNewDoc && r.newDocParent == "d1")
        // ⇧⌘I: Books, then the picker
        let before = r.addBooksRequest
        _ = r.perform(.addBooks, picker: picker)
        #expect(r.padItem == .books && r.addBooksRequest == before + 1)
        // the palette lists what applies, by words in any order
        let names = ActionsPalette.matching("doc new") { _ in true }.map(\.title)
        #expect(names.contains("New Doc") && names.contains("New Doc Inside This One") && !names.contains("Books"))
        #expect(ActionsPalette.matching("") { $0 != .exportPDF }.allSatisfy { $0.command != .exportPDF })
    }

    @Test func nothingRunsBeforeSignIn() {
        let r = Router()
        for c in [AppCommand.newDoc, .search, .refresh, .settings, .today, .todos, .workspace(1)] {
            #expect(!r.canPerform(c, signedIn: false, picker: picker), "\(c)")
            #expect(r.canPerform(c, signedIn: true, picker: picker), "\(c)")
        }
        #expect(!r.canPerform(.workspace(4), signedIn: true, picker: picker))
    }

    @Test func theDocInFront() {
        let r = Router()
        #expect(r.focusedDoc == nil)

        // phone: the top of the current tab's stack
        r.tab = .library
        r.libraryPath = [.doc("a"), .doc("b")]
        #expect(r.focusedDoc == "b")
        r.libraryPath.append(.todos)
        #expect(r.focusedDoc == nil)
        r.tab = .today
        #expect(r.focusedDoc == nil)

        // split view: a sidebar doc, or the doc pushed over it
        r.isPad = true
        r.select(.doc("c"))
        #expect(r.focusedDoc == "c")
        r.open(.doc("d"))
        #expect(r.focusedDoc == "d")
    }

    @Test func navigationCommandsMoveTheRightLayout() {
        let phone = Router()
        _ = phone.perform(.search, picker: picker)
        #expect(phone.tab == .search)
        #expect(phone.searchFocusRequest == 0, "the phone's Search tab focuses its own field")
        _ = phone.perform(.todos, picker: picker)
        #expect(phone.tab == .todos)
        _ = phone.perform(.today, picker: picker)
        #expect(phone.tab == .today)

        let pad = Router()
        pad.isPad = true
        pad.select(.doc("x"))
        pad.open(.doc("y"))
        _ = pad.perform(.search, picker: picker)
        #expect(pad.padItem == .search)
        #expect(pad.padPath.isEmpty)
        #expect(pad.searchFocusRequest == 1)
        _ = pad.perform(.todos, picker: picker)
        #expect(pad.padItem == .todos)
        _ = pad.perform(.today, picker: picker)
        #expect(pad.padItem == .today)
        #expect(pad.tab == .today, "the tab bar's state is left alone")
    }

    @Test func sheetsAndEffects() {
        let r = Router()
        r.newDocParent = "old"
        #expect(r.perform(.newDoc, picker: picker) == .none)
        #expect(r.showNewDoc)
        #expect(r.newDocParent == nil, "⌘N makes a doc at the root")
        #expect(r.perform(.settings, picker: picker) == .none)
        #expect(r.showSettings)
        #expect(r.perform(.refresh, picker: picker) == .refresh)
        #expect(r.perform(.workspace(2), picker: picker) == .selectWorkspace(.id("w1")))
        #expect(r.perform(.workspace(9), picker: picker) == .none)
    }
}

#if targetEnvironment(macCatalyst)
@Suite struct MacWindowTests {
    @Test func onlyAnUntouchedWindowIsResized() {
        #expect(MacWindow.size(replacing: CGSize(width: 1024, height: 768)) == MacWindow.initial)
        #expect(MacWindow.size(replacing: CGSize(width: 1400, height: 900)) == nil)
        #expect(MacWindow.initial.width >= MacWindow.minimum.width && MacWindow.initial.height >= MacWindow.minimum.height)
        // grown around the system's centre, never past the top-left corner
        let placed = CGRect(x: 223, y: 72, width: 1024, height: 768)
        #expect(MacWindow.grown(placed, to: MacWindow.initial) == CGRect(x: 95, y: 26, width: 1280, height: 860))
        #expect(MacWindow.grown(CGRect(x: 10, y: 0, width: 1024, height: 768), to: MacWindow.initial).origin == .zero)
    }
}
#endif
