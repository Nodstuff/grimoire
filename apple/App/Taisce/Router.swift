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

/// iPad sidebar selection.
enum PadItem: Hashable {
    case today, todos, search
    case doc(DocID)
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
