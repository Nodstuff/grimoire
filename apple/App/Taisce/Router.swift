import Foundation
import Observation
import TaisceKit

enum AppTab: Hashable {
    case today, library, todos, search
}

/// A pushed screen.
enum Route: Hashable {
    case doc(DocID)
    case todos
}

/// iPad and Mac sidebar selection.
enum PadItem: Hashable {
    case today, todos, search
    case doc(DocID)
}

/// A menu-bar command (the Mac's menus, an iPad's hardware keyboard).
enum AppCommand: Hashable {
    case newDoc, search, toggleEdit, refresh, settings, today, todos
    /// ⌘1…⌘9: the nth workspace in the switcher's order (1-based)
    case workspace(Int)
}

/// The part of a command the router can't do itself.
enum CommandEffect: Equatable {
    case none, refresh
    case selectWorkspace(WorkspaceScope)
}

/// ⌘E on the doc in front: start editing it, or finish.
struct EditRequest: Equatable {
    var doc: DocID
    var serial: Int
}

/// Which tab / sidebar item is showing and each stack's path. One per
/// window; screens push through it so wikilinks work from anywhere.
@MainActor @Observable
final class Router {
    var tab: AppTab = .today
    var todayPath: [Route] = []
    var libraryPath: [Route] = []
    var todosPath: [Route] = []
    var searchPath: [Route] = []

    var padItem: PadItem? = .today
    var padPath: [Route] = []
    var searchQuery = ""
    var showSettings = false
    /// the "New doc" sheet, and the folder it starts in
    var showNewDoc = false
    var newDocParent: DocID?
    /// set by the pad layout, so `open` pushes onto the detail stack
    var isPad = false
    /// the doc whose editor is open, as `DocScreen` reports it
    var editingDoc: DocID?
    /// ⌘E: the doc screen showing `doc` toggles edit mode
    private(set) var editRequest: EditRequest?
    /// ⌘F on the split view: bumped to focus the sidebar's search field
    private(set) var searchFocusRequest = 0

    func open(_ route: Route) {
        if isPad {
            padPath.append(route)
            return
        }
        switch tab {
        case .today: todayPath.append(route)
        case .library: libraryPath.append(route)
        case .todos: todosPath.append(route)
        case .search: searchPath.append(route)
        }
    }

    /// A sidebar doc replaces the detail stack; a doc linked from a doc pushes.
    func select(_ item: PadItem) {
        padItem = item
        padPath = []
    }

    func newDoc(in parent: DocID? = nil) {
        newDocParent = parent
        showNewDoc = true
    }

    func showTodos() {
        if isPad { select(.todos) } else { tab = .todos }
    }

    func showSearch() {
        if isPad { select(.search) } else { tab = .search }
    }

    /// The doc at the front of this window: the top of the visible stack.
    var focusedDoc: DocID? {
        let path: [Route]
        if isPad {
            if padPath.isEmpty, case .doc(let id)? = padItem { return id }
            path = padPath
        } else {
            switch tab {
            case .today: path = todayPath
            case .library: path = libraryPath
            case .todos: path = todosPath
            case .search: path = searchPath
            }
        }
        if case .doc(let id)? = path.last { return id }
        return nil
    }

    /// Whether a command applies now. Nothing works before sign-in; ⌘E
    /// needs a doc in front; ⌘n needs an nth workspace.
    func canPerform(_ command: AppCommand, signedIn: Bool, picker: WorkspacePicker) -> Bool {
        guard signedIn else { return false }
        switch command {
        case .toggleEdit: return editingDoc != nil || focusedDoc != nil
        case .workspace(let n): return picker.shortcut(n) != nil
        default: return true
        }
    }

    /// Do the navigation part of a command; the rest comes back as an effect.
    func perform(_ command: AppCommand, picker: WorkspacePicker) -> CommandEffect {
        switch command {
        case .newDoc:
            newDoc(in: nil)
        case .search:
            showSearch()
            if isPad { searchFocusRequest += 1 }
        case .toggleEdit:
            guard let doc = editingDoc ?? focusedDoc else { break }
            editRequest = EditRequest(doc: doc, serial: (editRequest?.serial ?? 0) + 1)
        case .refresh:
            return .refresh
        case .settings:
            showSettings = true
        case .today:
            if isPad { select(.today) } else { tab = .today }
        case .todos:
            showTodos()
        case .workspace(let n):
            if let scope = picker.shortcut(n) { return .selectWorkspace(scope) }
        }
        return .none
    }

    /// Development launch arguments, for screenshots and UI runs:
    /// `-tab today|library|todos|search`, `-openDoc <title>`,
    /// `-searchQuery <text>`, `-showSettings YES`. (`-serverURL <url>` is read
    /// by `AppModel` straight from UserDefaults' argument domain.)
    func applyLaunchArguments(index: DocIndex) {
        let d = UserDefaults.standard
        switch d.string(forKey: "tab") {
        case "library": tab = .library; padItem = .today
        case "todos": tab = .todos; padItem = .todos
        case "search": tab = .search; padItem = .search
        default: break
        }
        if let q = d.string(forKey: "searchQuery") { searchQuery = q }
        _ = openLaunchDoc(index: index)
        if d.bool(forKey: "showSettings") { showSettings = true }
    }

    private var openedLaunchDoc = false

    /// `-openDoc <title>`, once the tree has it (a doc made just before
    /// launch arrives with the first sync, after the cached tree). True
    /// when there is nothing (left) to open.
    func openLaunchDoc(index: DocIndex) -> Bool {
        guard !openedLaunchDoc, let title = UserDefaults.standard.string(forKey: "openDoc") else { return true }
        guard let doc = index.doc(titled: title) else { return false }
        openedLaunchDoc = true
        if isPad {
            padItem = .doc(doc.id)
        } else {
            open(.doc(doc.id))
        }
        return true
    }
}
