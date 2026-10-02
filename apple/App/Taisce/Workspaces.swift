import Foundation
import TaisceKit

/// What the switcher offers and which workspace is current. There is no
/// "All": the current one is the stored choice while it still exists, else
/// the first workspace, else Unsorted. Unsorted is offered only while it
/// has docs.
struct WorkspacePicker: Hashable, Sendable {
    var workspaces: [Workspace]
    /// docs resolving to no workspace
    var unsortedCount: Int
    var stored: WorkspaceScope?
    /// false on a daemon without workspaces: no switcher, no filter
    var enabled = true

    var ordered: [Workspace] { Workspace.ordered(workspaces) }

    var options: [WorkspaceScope] {
        ordered.map { .id($0.id) } + (unsortedCount > 0 ? [.unsorted] : [])
    }

    /// nil when workspaces are off (calls then carry no `workspace`).
    var current: WorkspaceScope? {
        guard enabled else { return nil }
        if let stored, options.contains(stored) { return stored }
        return ordered.first.map { .id($0.id) } ?? .unsorted
    }

    /// ⌘1…⌘9: the nth option (1-based); nil past the end or with workspaces off.
    func shortcut(_ n: Int) -> WorkspaceScope? {
        guard enabled, n >= 1, n <= options.count else { return nil }
        return options[n - 1]
    }

    func name(_ scope: WorkspaceScope) -> String {
        switch scope {
        case .unsorted: "Unsorted"
        case .id(let id): workspaces.first { $0.id == id }?.name ?? "Workspace"
        }
    }

    func color(_ scope: WorkspaceScope) -> String? {
        scope.workspaceID.flatMap { id in workspaces.first { $0.id == id }?.color }
    }
}

/// The current workspace's slice of the tree.
enum WorkspaceFilter {
    /// Docs resolving to `scope`; nil (workspaces off) keeps them all. A doc
    /// whose parent is in another workspace shows at the root.
    static func docs(_ docs: [DocInfo], in scope: WorkspaceScope?) -> [DocInfo] {
        guard let scope else { return docs }
        return docs.filter { $0.workspaceID == scope.workspaceID }
    }

    /// Whether a doc belongs in `scope` (nil = everywhere): offline search.
    static func keeps(_ doc: DocInfo?, scope: WorkspaceScope?) -> Bool {
        guard let scope else { return true }
        return doc.map { WorkspaceScope($0.workspaceID) == scope } ?? false
    }

    /// The server's choice of list (crates/daemon/src/todo.rs `find_todo`):
    /// Unsorted's is the unlabelled root To-do, a workspace's the shallowest
    /// To-do resolving to it; nil scope is the legacy root To-do.
    static func todoDoc(in docs: [DocInfo], index: DocIndex, scope: WorkspaceScope?) -> DocID? {
        let todos = docs.filter { $0.title == TodoParser.todoDocTitle }
        switch scope {
        case nil:
            return (todos.first { $0.parentID == nil && $0.workspaceID == nil } ?? todos.first { $0.parentID == nil })?.id
        case .unsorted:
            return todos.first { $0.parentID == nil && $0.workspaceID == nil }?.id
        case .id(let w):
            return todos.filter { $0.workspaceID == w }.min { index.ancestors(of: $0.id).count < index.ancestors(of: $1.id).count }?.id
        }
    }
}

/// The last workspace used, per server (the app opens in it).
struct WorkspacePreference {
    var defaults: UserDefaults = .standard
    let key: String

    func load() -> WorkspaceScope? {
        defaults.string(forKey: key).map(WorkspaceScope.init(param:))
    }

    func save(_ scope: WorkspaceScope) {
        defaults.set(scope.param, forKey: key)
    }
}

/// The small coloured chip in a doc's header.
struct WorkspaceBadge: Hashable, Sendable {
    var name: String
    var color: String?
}

/// What an empty workspace says.
struct WorkspaceEmptyState: Hashable, Sendable {
    var title: String
    var hint: String

    init(name: String) {
        title = "Nothing in \(name) yet"
        hint = "Move docs here with \u{201C}Move to workspace\u{2026}\u{201D} in a doc\u{2019}s \u{2026} menu, or long-press one in Library."
    }
}

/// The six swatches a workspace can wear, stored as Theme token names so
/// they follow light/dark mode. Any other value is read as `#rrggbb`.
enum WorkspacePalette {
    static let colors = ["accent", "green", "amber", "rose", "accentActive", "secondary"]

    /// Builds before tokens stored the swatches' dark-mode hex: read those
    /// as their token, so they adapt too.
    static let legacyHex = [
        "#8b9dc3": "accent", "#95c99b": "green", "#d9b47a": "amber",
        "#d98a94": "rose", "#a9b7d6": "accentActive", "#8f8f9e": "secondary",
    ]

    /// The token a stored colour means, if any.
    static func token(_ stored: String?) -> String? {
        guard let stored else { return nil }
        if colors.contains(stored) { return stored }
        return legacyHex[stored.lowercased()]
    }

    /// For VoiceOver.
    static func name(_ token: String) -> String {
        switch token {
        case "accent": "Blue"
        case "green": "Green"
        case "amber": "Amber"
        case "rose": "Rose"
        case "accentActive": "Sky"
        case "secondary": "Grey"
        default: token
        }
    }
}

/// "Manage workspaces": the pure parts.
enum WorkspaceManagement {
    /// The inline delete confirmation.
    static func deletePrompt(_ w: Workspace) -> String {
        let docs = w.docCount == 1 ? "Its 1 doc moves" : "Its \(w.docCount) docs move"
        return "Delete \(w.name)? \(docs) to Unsorted. No docs are deleted."
    }

    /// The sort key that puts the workspace at `index` of the sidebar order
    /// (an index into the list WITHOUT it), between its new neighbours.
    static func sortKey(moving id: WorkspaceID, to index: Int, in all: [Workspace]) -> String {
        let rest = Workspace.ordered(all).filter { $0.id != id }
        let i = max(0, min(index, rest.count))
        let before = i > 0 ? rest[i - 1].sortKey : nil
        let after = i < rest.count ? rest[i].sortKey : nil
        return OrderKey.between(before, after)
    }

    /// `List.onMove`'s (source, destination) as an index into the list without it.
    static func destination(from source: Int, to destination: Int) -> Int {
        destination > source ? destination - 1 : destination
    }

    /// Where the app goes when `deleted` was current: the first remaining
    /// workspace, else Unsorted.
    static func fallback(afterDeleting deleted: WorkspaceID, from all: [Workspace]) -> WorkspaceScope {
        Workspace.ordered(all).first { $0.id != deleted }.map { .id($0.id) } ?? .unsorted
    }
}

extension AppModel {
    var hasWorkspaces: Bool { workspacesSupported || !workspaces.isEmpty }

    var workspacePicker: WorkspacePicker {
        WorkspacePicker(workspaces: workspaces, unsortedCount: unsortedDocCount, stored: storedWorkspace, enabled: hasWorkspaces)
    }

    /// nil on a daemon without workspaces.
    var currentWorkspace: WorkspaceScope? { workspacePicker.current }

    /// This device's clock, addressed to the current workspace's list.
    func todoClock(now: Date = .now) -> TodoClock {
        TodoClock(now: now).in(currentWorkspace)
    }

    /// Pinned docs in the current workspace, in pin order.
    var workspacePins: [DocID] {
        guard let scope = currentWorkspace else { return pins }
        return pins.filter { id in index.byID[id].map { WorkspaceScope($0.workspaceID) == scope } ?? false }
    }

    /// The chip for a doc's header (nil when workspaces are off).
    func workspaceBadge(for id: DocID) -> WorkspaceBadge? {
        guard hasWorkspaces, let doc = index.byID[id] else { return nil }
        let scope = WorkspaceScope(doc.workspaceID)
        return WorkspaceBadge(name: workspacePicker.name(scope), color: workspacePicker.color(scope))
    }

    /// Library's empty state for the current workspace.
    var workspaceEmptyState: WorkspaceEmptyState? {
        guard let scope = currentWorkspace, scope != .unsorted else { return nil }
        return WorkspaceEmptyState(name: workspacePicker.name(scope))
    }

    func loadCachedWorkspaces(_ cache: Cache) async {
        workspaces = (try? await cache.workspaces()) ?? []
        workspacesSupported = false
        storedWorkspace = WorkspacePreference(key: workspaceKey).load()
    }

    /// Re-read `GET /api/workspaces` (single flight). A daemon without the
    /// route leaves workspaces off.
    func refreshWorkspaces() {
        guard workspacesTask == nil, let api, let cache else { return }
        workspacesTask = Task { [weak self] in
            defer { self?.workspacesTask = nil }
            do {
                let list = try await api.workspaces()
                try? await cache.replaceWorkspaces(list.workspaces)
                self?.applyWorkspaces(list.workspaces)
            } catch APIError.notFound, APIError.notAPIRoute {
                self?.workspacesSupported = false
            } catch {}
        }
    }

    private func applyWorkspaces(_ all: [Workspace]) {
        let before = currentWorkspace
        workspaces = all
        workspacesSupported = true
        // a fallback (the stored one vanished) is not a choice: keep the stored one
        if currentWorkspace != before { workspaceDidChange() }
    }

    /// "New workspace…": create it and switch to it. Returns an error to show.
    func createWorkspace(name: String, color: String?) async -> String? {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return "Give it a name." }
        guard let api else { return "Not connected." }
        do {
            let w = try await api.createWorkspace(name: trimmed, color: color)
            workspaces.removeAll { $0.id == w.id }
            workspaces.append(w)
            workspacesSupported = true
            try? await cache?.replaceWorkspaces(workspaces)
            selectWorkspace(.id(w.id))
            return nil
        } catch APIError.server(let msg) {
            return msg
        } catch {
            return error.localizedDescription
        }
    }

    /// "Move to…": label each doc (its subtree follows) through the outbox,
    /// each with its own request_id; the tree rows arrive through sync.
    func moveDocs(_ ids: [DocID], to scope: WorkspaceScope) async {
        guard let cache else { return }
        do {
            for id in ids { try await cache.enqueueDocWorkspace(id, workspaceID: scope.workspaceID) }
        } catch {
            lastError = error.localizedDescription
        }
        await replayOutbox()
    }

    // MARK: manage

    /// Rename / recolour / reorder (PATCH); the local list follows the answer.
    func updateWorkspace(_ id: WorkspaceID, name: String? = nil, color: String? = nil, sortKey: String? = nil) async -> String? {
        guard let api else { return "Not connected." }
        do {
            let w = try await api.updateWorkspace(id, name: name, color: color, sortKey: sortKey)
            if let i = workspaces.firstIndex(where: { $0.id == id }) { workspaces[i] = w } else { workspaces.append(w) }
            try? await cache?.replaceWorkspaces(workspaces)
            return nil
        } catch APIError.server(let msg) {
            return msg
        } catch {
            return error.localizedDescription
        }
    }

    /// Drag to reorder: a key between the new neighbours.
    func moveWorkspace(_ id: WorkspaceID, to index: Int) async -> String? {
        await updateWorkspace(id, sortKey: WorkspaceManagement.sortKey(moving: id, to: index, in: workspaces))
    }

    /// The labels go, the docs stay. Deleting the current one switches to
    /// the first remaining workspace.
    func deleteWorkspace(_ id: WorkspaceID) async -> String? {
        guard let api else { return "Not connected." }
        do {
            try await api.deleteWorkspace(id)
        } catch APIError.server(let msg) {
            return msg
        } catch {
            return error.localizedDescription
        }
        let wasCurrent = currentWorkspace == .id(id)
        let fallback = WorkspaceManagement.fallback(afterDeleting: id, from: workspaces)
        workspaces.removeAll { $0.id == id }
        try? await cache?.replaceWorkspaces(workspaces)
        // its docs re-resolve now (Unsorted or an outer workspace): nothing vanishes
        try? await cache?.clearWorkspace(id)
        if wasCurrent { selectWorkspace(fallback) } else { workspaceDidChange() }
        refreshWorkspaces()
        return nil
    }

    /// Unsorted docs, flat, for the triage list.
    var unsortedDocs: [DocInfo] {
        docs.filter { $0.workspaceID == nil }.sorted { ($0.title.lowercased(), $0.id) < ($1.title.lowercased(), $1.id) }
    }
}
