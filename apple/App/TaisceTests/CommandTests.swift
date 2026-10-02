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

    @Test func nothingRunsBeforeSignIn() {
        let r = Router()
        for c in [AppCommand.newDoc, .search, .refresh, .settings, .today, .todos, .workspace(1)] {
            #expect(!r.canPerform(c, signedIn: false, picker: picker), "\(c)")
            #expect(r.canPerform(c, signedIn: true, picker: picker), "\(c)")
        }
        #expect(!r.canPerform(.workspace(4), signedIn: true, picker: picker))
    }

    @Test func editNeedsADocInFront() {
        let r = Router()
        #expect(r.focusedDoc == nil)
        #expect(!r.canPerform(.toggleEdit, signedIn: true, picker: picker))
        #expect(r.perform(.toggleEdit, picker: picker) == .none)
        #expect(r.editRequest == nil)

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
        #expect(r.canPerform(.toggleEdit, signedIn: true, picker: picker))
        _ = r.perform(.toggleEdit, picker: picker)
        #expect(r.editRequest == EditRequest(doc: "d", serial: 1))
        _ = r.perform(.toggleEdit, picker: picker)
        #expect(r.editRequest == EditRequest(doc: "d", serial: 2), "a second ⌘E is a new request")
    }

    @Test func doneGoesToTheDocBeingEditedEvenIfAnotherIsInFront() {
        let r = Router()
        r.isPad = true
        r.select(.today)
        r.editingDoc = "e"
        #expect(r.canPerform(.toggleEdit, signedIn: true, picker: picker))
        _ = r.perform(.toggleEdit, picker: picker)
        #expect(r.editRequest?.doc == "e")
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
    }
}
#endif
