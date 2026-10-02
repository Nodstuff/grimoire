import Foundation
import Testing
import TaisceKit
@testable import Taisce

// ADR 0004: one server, several people.
private let mine = Workspace(id: "w1", name: "Work", sortKey: "b", role: "owner", ownerName: "Tom", displayName: "Work")
private let homeShared = Workspace(id: "w4", name: "Home", sortKey: "c", role: "owner", ownerName: "Tom", displayName: "Home", shared: true)
private let theirs = Workspace(id: "w2", name: "Work", sortKey: "a", role: "editor", ownerName: "Aoife", displayName: "Work · Aoife", shared: true)
private let viewed = Workspace(id: "w3", name: "Family", sortKey: "0", role: "viewer", ownerName: "Aoife", displayName: "Family", shared: true)
private let all = [theirs, viewed, mine, homeShared]

@MainActor @Suite struct MultiUserTests {
    // MARK: 1. the switcher

    @Test func theSwitcherShowsDisplayNamesOwnFirst() {
        let picker = WorkspacePicker(workspaces: all, unsortedCount: 2, stored: nil)
        // own (by sort key), then shared with you (by sort key), then Unsorted
        #expect(picker.options == [.id("w1"), .id("w4"), .id("w3"), .id("w2"), .unsorted])
        #expect(picker.name(.id("w2")) == "Work · Aoife")
        #expect(picker.name(.id("w1")) == "Work")
        #expect(picker.current == .id("w1"), "the first of your own")
        // ⌘1…⌘9 follow the same order
        #expect(picker.shortcut(3) == .id("w3"))
    }

    @Test func anOlderServersWorkspacesFallBackToTheirNames() throws {
        let old = try JSONDecoder().decode(Workspace.self, from: Data(#"{"id":"w9","name":"Legacy","doc_count":1}"#.utf8))
        let picker = WorkspacePicker(workspaces: [old], unsortedCount: 0, stored: nil)
        #expect(picker.name(.id("w9")) == "Legacy")
        #expect(!picker.isShared(.id("w9")))
        #expect(picker.accessibilityName(.id("w9")) == "Legacy")
        #expect(picker.menuTitle(.id("w9")) == "Legacy")
        #expect(EditAccess.forDoc(workspaceID: "w9", workspaces: [old]) == .full)
    }

    @Test func sharedWorkspacesAreMarkedAndSaySo() {
        let picker = WorkspacePicker(workspaces: all, unsortedCount: 0, stored: nil)
        #expect(picker.isShared(.id("w3")) && picker.isShared(.id("w4")) && !picker.isShared(.id("w1")))
        #expect(!picker.isShared(.unsorted))
        #expect(picker.accessibilityName(.id("w3")) == "Family, shared by Aoife, view only")
        #expect(picker.accessibilityName(.id("w2")) == "Work · Aoife, shared by Aoife")
        #expect(picker.accessibilityName(.id("w4")) == "Home, shared", "your own, shared out")
        #expect(picker.accessibilityName(.id("w1")) == "Work")
        #expect(picker.menuTitle(.id("w3")) == "Family (shared)")
        #expect(WorkspaceSharedGlyph.symbol == "person.2")
    }

    // MARK: 2. a viewer reads only

    @Test func editAccessFollowsTheRole() {
        #expect(EditAccess.forDoc(workspaceID: nil, workspaces: all) == .full, "your Unsorted")
        #expect(EditAccess.forDoc(workspaceID: "w1", workspaces: all) == .full, "your workspace")
        #expect(EditAccess.forDoc(workspaceID: "w4", workspaces: all) == .full, "yours, shared out")
        let viewer = EditAccess.forDoc(workspaceID: "w3", workspaces: all)
        #expect(!viewer.canEdit && !viewer.canCreate && !viewer.canMove)
        #expect(viewer.note == "View only")
        let editor = EditAccess.forDoc(workspaceID: "w2", workspaces: all)
        #expect(editor.canEdit && editor.canCreate && editor.note == nil)
        #expect(!editor.canMove, "only the owner moves docs out of a workspace")
        // a workspace not (yet) known here: the server decides, the outbox reports
        #expect(EditAccess.forDoc(workspaceID: "w-unknown", workspaces: all) == .full)
        #expect(EditAccess.forScope(nil, workspaces: all) == .full, "workspaces off")
        #expect(EditAccess.forScope(.id("w3"), workspaces: all) == .viewOnly)
        #expect(EditAccess.forScope(.unsorted, workspaces: all) == .full)
        // never move into a workspace you only view
        #expect(EditAccess.moveTargets(all).map(\.id) == ["w1", "w4", "w2"])
    }

    @Test func commandsRespectTheRole() {
        let picker = WorkspacePicker(workspaces: all, unsortedCount: 0, stored: .id("w3"))
        let r = Router()
        r.open(.doc("viewed-doc"))
        let canEdit: (DocID) -> Bool = { $0 != "viewed-doc" }
        #expect(!r.canPerform(.toggleEdit, signedIn: true, picker: picker, canEdit: canEdit), "⌘E on a viewer's doc")
        r.open(.doc("own-doc"))
        #expect(r.canPerform(.toggleEdit, signedIn: true, picker: picker, canEdit: canEdit))
        // finishing an edit is always allowed (made a viewer mid-edit)
        r.editingDoc = "viewed-doc"
        #expect(r.canPerform(.toggleEdit, signedIn: true, picker: picker, canEdit: { _ in false }))
        #expect(!r.canPerform(.newDoc, signedIn: true, picker: picker, canCreate: false))
        #expect(r.canPerform(.newDoc, signedIn: true, picker: picker, canCreate: true))
        #expect(r.canPerform(.search, signedIn: true, picker: picker, canEdit: { _ in false }, canCreate: false), "reading is never gated")
    }

    @Test func aViewersToDosSayViewOnly() {
        #expect(TodosContent.kicker(offline: false, readOnly: true) == "View only")
        #expect(TodosContent.kicker(offline: true, readOnly: true) == "View only · offline")
        #expect(TodosContent.kicker(offline: true, readOnly: false) == "Offline · from this device")
        #expect(TodosContent.kicker(offline: false, readOnly: false) == nil)
    }

    @Test func alertsComeOnlyFromYourOwnLists() async throws {
        let cache = try Cache.inMemory()
        let lists: [(DocID, WorkspaceID?)] = [("t-unsorted", nil), ("t-mine", "w1"), ("t-family", "w3"), ("t-theirs", "w2")]
        for (id, _) in lists {
            try await cache.storeDoc(DocTree(
                doc: DocSummary(id: id, parentID: nil, title: "To-do", currentEpoch: 1),
                roots: [BlockNode(block: Block(id: "\(id)-b", docID: id, parentID: nil, orderKey: "i", blockType: .paragraph, content: "## 2026-10-02\n\n- [ ] item \(id) · due 2026-10-03"))]
            ))
        }
        try await cache.replaceTree(lists.map { DocSummary(id: $0.0, parentID: nil, title: "To-do", currentEpoch: 1, workspaceID: $0.1) })
        let records = try await cache.todos()
        let docs = try await cache.docs()
        #expect(records.count == 4)
        let kept = Set(NotificationCoordinator.alerting(records, docs: docs, workspaces: all).map(\.docID))
        #expect(kept == ["t-unsorted", "t-mine"], "a shared workspace's list (someone else's) never alerts here")
        #expect(NotificationCoordinator.alerting(records, docs: docs, workspaces: []).count == 4, "an older server: all")
    }

    // MARK: 3. access revoked

    @Test func anUnsharedDocLeavesEveryStack() {
        let r = Router()
        r.libraryPath = [.doc("parent"), .doc("gone"), .doc("child-of-gone")]
        r.todayPath = [.doc("kept")]
        r.isPad = true
        r.padItem = .doc("gone")
        r.padPath = [.doc("gone2")]
        #expect(r.visibleDocs == ["parent", "gone", "child-of-gone", "kept", "gone2"])
        #expect(r.drop(["gone"]), "it was showing")
        #expect(r.libraryPath == [.doc("parent")], "cut where the dropped doc was")
        #expect(r.todayPath == [.doc("kept")])
        #expect(r.padItem == .today && r.padPath.isEmpty)
        #expect(!r.drop(["never-open"]), "nothing to close, no note")
        #expect(AppModel.revokedNotice == "This doc is no longer shared with you.")
    }

    // MARK: 4. one person's cache

    func settings() -> (UserDefaults, String) {
        let name = "taisce.tests.\(UUID().uuidString)"
        return (UserDefaults(suiteName: name)!, name)
    }

    @Test func signOutForgetsThePersonsSettings() {
        let (d, name) = settings()
        defer { UserDefaults.standard.removePersistentDomain(forName: name) }
        let s = UserSettings(defaults: d, server: "https://taisce.null.ie")
        d.set(["d1"], forKey: s.pinsKey)
        d.set("w1", forKey: s.workspaceKey)
        d.set("d1,d2", forKey: UserSettings.libraryExpandedKey)
        d.set("https://taisce.null.ie", forKey: AppModel.serverURLKey)
        #expect(s.pinsKey == "pins-taisce.null.ie-0" && s.workspaceKey == "workspace-taisce.null.ie-0", "the keys earlier builds used")
        s.forget()
        #expect(d.object(forKey: s.pinsKey) == nil && d.object(forKey: s.workspaceKey) == nil)
        #expect(d.object(forKey: UserSettings.libraryExpandedKey) == nil)
        #expect(d.string(forKey: AppModel.serverURLKey) == "https://taisce.null.ie", "the server is the device's, not the person's")
    }

    @Test func appLinkHostsComeFromInfoPlist() {
        #expect(AppLinks.hosts([AppLinks.infoKey: "taisce.null.ie"]) == ["taisce.null.ie"])
        #expect(AppLinks.hosts([AppLinks.infoKey: ""]).isEmpty, "Debug: none")
        #expect(AppLinks.hosts([AppLinks.infoKey: "$(TAISCE_APP_LINK_HOST)"]).isEmpty, "an unexpanded setting is no host")
        #expect(AppLinks.hosts([:]).isEmpty)
        // this test host is a Debug build: the custom scheme, no Associated Domains
        #expect(AppLinks.hosts().isEmpty)
    }

    // MARK: 5. history

    @Test func anotherPersonsAgentIsNamedInFull() {
        let rows = [DocHistoryEntry(opID: "0199a0b0-c0d0-7abc-8def-0123456789ab", principalName: "claude:tagger (Aoife)", principalKind: "agent")]
        #expect(EditMeta(history: rows)?.author == "claude:tagger (Aoife)")
    }
}

/// The model end to end against a cache on disk: sign-out wipes it and the
/// person's settings, on the iPhone (data protection kept) and the Mac.
@MainActor @Suite(.serialized) struct SignOutWipeTests {
    @Test func signOutWipesTheCacheOutboxPinsAndWorkspace() async throws {
        UserDefaults.standard.set("http://127.0.0.1:29", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel()
        m.discover = { _ in nil }
        await m.boot()
        await m.stopSync()
        let cache = try #require(m.cache)
        let id = "wipe-\(UUID().uuidString.prefix(8))"
        try await cache.storeDoc(DocTree(
            doc: DocSummary(id: id, parentID: nil, title: "Private", currentEpoch: 1),
            roots: [BlockNode(block: Block(id: "\(id)-b", docID: id, parentID: nil, orderKey: "i", blockType: .paragraph, content: "zebraprivate words"))]
        ))
        try await cache.enqueue(ProposeRequest(docID: id, baseEpoch: 1, ops: [.replace(target: "\(id)-b", content: "queued")]))
        try await cache.replaceWorkspaces([Workspace(id: "w1", name: "Work")])
        m.togglePin(id)
        m.selectWorkspace(.id("w1"))
        let settings = UserSettings(server: m.serverURL)
        #expect(UserDefaults.standard.stringArray(forKey: settings.pinsKey) == [id])
        #expect(UserDefaults.standard.string(forKey: settings.workspaceKey) == "w1")

        await m.signOut()

        #expect(try await cache.docs().isEmpty)
        #expect(try await cache.pendingOutbox().isEmpty)
        #expect(try await cache.searchBlocks("zebraprivate").isEmpty)
        #expect(try await cache.workspaces().isEmpty)
        #expect(m.pins.isEmpty && m.storedWorkspace == nil && m.workspaces.isEmpty)
        #expect(UserDefaults.standard.object(forKey: settings.pinsKey) == nil)
        #expect(UserDefaults.standard.object(forKey: settings.workspaceKey) == nil)
    }

    @Test func signOutAsksBeforeLosingUnsentChanges() {
        #expect(SignOutCheck.prompt(unsent: 0) == nil, "nothing to lose: sign out at once")
        #expect(SignOutCheck.prompt(unsent: 1) == "1 change hasn't been sent and will be lost.")
        #expect(SignOutCheck.prompt(unsent: 3) == "3 changes haven't been sent and will be lost.")
        #expect(SignOutCheck.confirm == "Sign out anyway" && SignOutCheck.cancel == "Cancel")
    }

    @Test func unsentChangesCountQueuedAndRefusedAfterALastTry() async throws {
        // nothing listens on 127.0.0.1:29: the last replay can't send
        UserDefaults.standard.set("http://127.0.0.1:29", forKey: AppModel.serverURLKey)
        defer { UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey) }
        let m = AppModel()
        m.discover = { _ in nil }
        await m.boot()
        await m.stopSync()
        let cache = try #require(m.cache)
        try await cache.wipe()
        #expect(await m.unsentChangesBeforeSignOut() == 0)
        let queued = try await cache.enqueue(ProposeRequest(docID: "d", baseEpoch: 1, ops: [.replace(target: "b", content: "typed")]))
        let refused = try await cache.enqueue(ProposeRequest(docID: "d", baseEpoch: 1, ops: [.replace(target: "c", content: "refused")]))
        try await cache.markOutbox(try #require(refused.id), state: .failed, error: "x")
        #expect(await m.unsentChangesBeforeSignOut() == 2, "one still queued after the try, one refused")
        #expect(try await cache.outboxEntry(try #require(queued.id))?.state == .pending, "the try kept it")
        try await cache.wipe()
    }

    @Test func aWipedCacheKeepsItsProtectionClassAndLeavesNoText() async throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "wipe-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: dir) }
        let path = dir.appending(path: "cache.sqlite").path(percentEncoded: false)
        let cache = try Cache(path: path)
        try await cache.storeDoc(DocTree(
            doc: DocSummary(id: "p", parentID: nil, title: "Private", currentEpoch: 1),
            roots: [BlockNode(block: Block(id: "p-b", docID: "p", parentID: nil, orderKey: "i", blockType: .paragraph, content: "zebrawipemarker"))]
        ))
        try await cache.wipe()
        for file in [path, path + "-wal", path + "-shm"] where FileManager.default.fileExists(atPath: file) {
            let bytes = try Data(contentsOf: URL(filePath: file))
            #expect(bytes.range(of: Data("zebrawipemarker".utf8)) == nil, "\(file)")
        }
        #if targetEnvironment(macCatalyst)
        // the Mac sets no class (sandbox container + FileVault); the wipe must not try
        #expect(!Cache.appliesFileProtection)
        #else
        #expect(Cache.appliesFileProtection)
        for file in [path, path + "-wal", path + "-shm"] where FileManager.default.fileExists(atPath: file) {
            let p = try URL(filePath: file).resourceValues(forKeys: [.fileProtectionKey]).fileProtection
            #expect(p == .completeUntilFirstUserAuthentication, "\(file)")
        }
        #endif
    }
}
