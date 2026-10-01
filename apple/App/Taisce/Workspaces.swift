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

/// A colour offered by "New workspace…".
enum WorkspacePalette {
    static let colors = ["#5b8def", "#3fb68b", "#e0a93b", "#e5655c", "#a77bd9", "#3bb3c3", "#8a8f98"]
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

    /// Unsorted docs, flat, for the triage list.
    var unsortedDocs: [DocInfo] {
        docs.filter { $0.workspaceID == nil }.sorted { ($0.title.lowercased(), $0.id) < ($1.title.lowercased(), $1.id) }
    }
}
