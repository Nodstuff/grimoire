import Foundation
import Observation
import SwiftUI
import TaisceKit

enum AppTab: Hashable {
    case today, library, books, todos, search
}

/// A pushed screen.
enum Route: Hashable {
    case doc(DocID)
    case todos
    /// the review queue (Today's Review card)
    case review
    /// a book's details (`BookDetail`), and its reader (`BookReaderScreen`)
    case book(String)
    case bookReader(String)
}

/// A book to open in the reader (`Router.readingBook`).
struct ReadingTarget: Identifiable, Hashable {
    let id: String
}

/// iPad and Mac sidebar selection.
enum PadItem: Hashable {
    case today, todos, review, books, search
    case doc(DocID)
}

/// A menu-bar command (the Mac's menus, an iPad's hardware keyboard).
enum AppCommand: Hashable {
    case newDoc, newFolder, search, refresh, settings, today, todos
    /// ⌘O: open a doc by title (Search, docs, the field focused)
    case open
    /// ⌘K: the actions palette
    case actions
    /// ⌘\ (and ⌘T, the web's tree key): show or hide the sidebar (split view)
    case toggleSidebar
    /// ⌥⌘N: a new doc inside the doc in front
    case newDocHere
    /// ⇧⌘I: Books, then the EPUB picker
    case addBooks
    case books, review
    /// ⌘[ / ⌘]: back and forward through what the split view showed
    case back, forward
    /// ⌘F: find in the doc in front (Decision 2: ⇧⌘F is Search)
    case findInDoc
    /// the doc in front: Share link… and Export as PDF… (SERVER mode)
    case shareLink, exportPDF
    /// ⌘1…⌘9: the nth workspace in the switcher's order (1-based)
    case workspace(Int)
}

/// The part of a command the router can't do itself.
enum CommandEffect: Equatable {
    case none, refresh
    case selectWorkspace(WorkspaceScope)
}

/// Something the doc screen showing `doc` should open: the share sheet,
/// a PDF export, or its link comments (a push tap).
struct DocAction: Equatable {
    enum Kind: Equatable { case shareLink, exportPDF, linkComments, find }
    var doc: DocID
    var kind: Kind
    var serial: Int
    var at: Date = .now

    /// How long a request waits for its doc's screen.
    static let lifetime: TimeInterval = 15

    func isFresh(now: Date = .now) -> Bool { now.timeIntervalSince(at) < Self.lifetime }
}

/// Which tab / sidebar item is showing and each stack's path. One per
/// window; screens push through it so wikilinks work from anywhere.
@MainActor @Observable
final class Router {
    var tab: AppTab = .today
    /// what an open doc does before it is snapshotted (Share link, Export
    /// as PDF): its surface writes what's typed to the outbox first
    @ObservationIgnored var snapshotPrep: [DocID: @MainActor () async -> Void] = [:]

    /// Run the open doc's preparation, if it has one.
    @MainActor func prepareSnapshot(_ doc: DocID) async {
        await snapshotPrep[doc]?()
    }
    var todayPath: [Route] = []
    var libraryPath: [Route] = []
    var todosPath: [Route] = []
    var searchPath: [Route] = []
    var booksPath: [Route] = []

    var padItem: PadItem? = .today
    /// the shelf Books shows on iPad and the Mac (the sidebar's Shelves and Series rows)
    var bookShelf: BookShelfFilter = .all
    var padPath: [Route] = []
    var searchQuery = ""
    var showSettings = false
    /// the "New doc" sheet, and the folder it starts in
    var showNewDoc = false
    var newDocParent: DocID?
    /// the "New folder" name prompt, and where the folder goes (nil = top level)
    var showNewFolder = false
    var newFolderParent: DocID?
    /// a doc the Library should bring into view (a folder just made): its
    /// folders open, the tree scrolls to it
    var revealDoc: DocID?
    /// set by the pad layout, so `open` pushes onto the detail stack
    var isPad = false
    /// a doc action waiting for that doc's screen (it clears it when done)
    var docAction: DocAction?
    /// ⌘F on the split view: bumped to focus the sidebar's search field
    private(set) var searchFocusRequest = 0
    /// ⌘O: Search should show docs (set with the focus request)
    var searchScopeRequest: SearchScope?
    /// ⌘K: the actions palette
    var showActions = false
    /// the split view's columns (⌘\\ hides or shows the sidebar); the split
    /// view writes it too when the person drags the sidebar away
    var sidebarHidden = false
    var columnVisibility: NavigationSplitViewVisibility {
        get { sidebarHidden ? .detailOnly : .automatic }
        set { sidebarHidden = newValue == .detailOnly }
    }
    /// ⇧⌘I: bumped for Books to open its EPUB picker
    private(set) var addBooksRequest = 0

    func open(_ route: Route) {
        if isPad {
            remember()
            padPath.append(route)
            return
        }
        switch tab {
        case .today: todayPath.append(route)
        case .library: libraryPath.append(route)
        case .todos: todosPath.append(route)
        case .search: searchPath.append(route)
        case .books: booksPath.append(route)
        }
    }

    /// A sidebar doc replaces the detail stack; a doc linked from a doc pushes.
    func select(_ item: PadItem) {
        remember()
        padItem = item
        padPath = []
    }

    // MARK: back and forward (the split view: the Mac's and iPad's toolbar, ⌘[ ⌘])

    struct Place: Equatable {
        var item: PadItem?
        var path: [Route]
    }

    private(set) var backPlaces: [Place] = []
    private(set) var forwardPlaces: [Place] = []
    var canGoBack: Bool { !backPlaces.isEmpty }
    var canGoForward: Bool { !forwardPlaces.isEmpty }
    private var here: Place { Place(item: padItem, path: padPath) }

    /// Before the split view moves: where it was, for Back (a new move clears Forward).
    private func remember() {
        guard isPad else { return }
        if backPlaces.last != here { backPlaces.append(here) }
        if backPlaces.count > 50 { backPlaces.removeFirst() }
        forwardPlaces = []
    }

    func goBack() {
        // a stack's own Back (the system's) may already have left a place: skip those
        while let last = backPlaces.last, last == here { backPlaces.removeLast() }
        guard let place = backPlaces.popLast() else { return }
        forwardPlaces.append(here)
        padItem = place.item
        padPath = place.path
    }

    func goForward() {
        guard let place = forwardPlaces.popLast() else { return }
        backPlaces.append(here)
        padItem = place.item
        padPath = place.path
    }

    /// What the sidebar highlights: the screen in front, not the item the
    /// stack started from. With a doc pushed over Today, Today is no longer
    /// selected, so clicking it is a change and pops back (the List drops
    /// a click on the row that is already selected).
    var sidebarSelection: PadItem? {
        switch padPath.last {
        case .doc(let id)?: .doc(id)
        case .todos?: .todos
        case .review?: .review
        case .book?, .bookReader?: .books
        case nil: padItem
        }
    }

    func newDoc(in parent: DocID? = nil) {
        newDocParent = parent
        showNewDoc = true
    }

    func newFolder(in parent: DocID? = nil) {
        newFolderParent = parent
        showNewFolder = true
    }

    func showTodos() {
        if isPad { select(.todos) } else { tab = .todos }
    }

    func showSearch() {
        if isPad { select(.search) } else { tab = .search }
    }

    /// Put the cursor in Search's field (the split view's Search screen).
    func focusSearch() {
        searchFocusRequest += 1
    }

    /// The review queue: the sidebar's Review, or pushed over Today on a phone.
    func showReview() {
        if isPad { select(.review) } else { tab = .today; todayPath = [.review] }
    }

    func showBooks() {
        if isPad { select(.books) } else { tab = .books }
    }

    /// The book open in the reader, full screen over the window (iPad and
    /// Mac included: a reader squeezed beside the sidebar cut its spread).
    var readingBook: ReadingTarget?

    /// A book's reader (Today's Continue reading): Books underneath, so
    /// closing the reader lands there.
    func readBook(_ id: String) {
        if isPad {
            select(.books)
        } else {
            tab = .books
            booksPath = []
        }
        readingBook = ReadingTarget(id: id)
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
            case .books: path = booksPath
            }
        }
        if case .doc(let id)? = path.last { return id }
        return nil
    }

    /// Whether a command applies now. Nothing works before sign-in; Share
    /// link needs a doc in front that you may edit (ADR 0004: not a
    /// viewer's); ⌘N needs a workspace you may add to; ⌘n needs an nth workspace.
    func canPerform(_ command: AppCommand, signedIn: Bool, picker: WorkspacePicker, canEdit: (DocID) -> Bool = { _ in true }, canCreate: Bool = true, shares: Bool = false) -> Bool {
        guard signedIn else { return false }
        switch command {
        case .shareLink:
            // SERVER mode only, a doc in front you may edit (owner or editor)
            return shares && (focusedDoc.map(canEdit) ?? false)
        case .exportPDF:
            // reading is enough to print
            return shares && focusedDoc != nil
        case .newDoc, .newFolder, .addBooks: return canCreate
        case .newDocHere: return canCreate && focusedDoc != nil
        case .toggleSidebar: return isPad
        case .back: return isPad && canGoBack
        case .forward: return isPad && canGoForward
        case .findInDoc: return focusedDoc != nil
        case .workspace(let n): return picker.shortcut(n) != nil
        default: return true
        }
    }

    /// ADR 0004: docs no longer shared with you leave every stack (and the
    /// sidebar's selection). True when one was showing in this window.
    @discardableResult
    func drop(_ docs: Set<DocID>) -> Bool {
        guard !docs.isEmpty else { return false }
        let shown = docs.contains { visibleDocs.contains($0) }
        func keep(_ r: Route) -> Bool { if case .doc(let id) = r { return !docs.contains(id) }; return true }
        // a stack is cut at the first dropped doc: what was pushed above it came from it
        func cut(_ path: [Route]) -> [Route] { Array(path.prefix { keep($0) }) }
        todayPath = cut(todayPath)
        libraryPath = cut(libraryPath)
        todosPath = cut(todosPath)
        searchPath = cut(searchPath)
        booksPath = cut(booksPath)
        padPath = cut(padPath)
        if case .doc(let id)? = padItem, docs.contains(id) {
            padItem = .today
            padPath = []
        }
        return shown
    }

    /// Every doc on a stack in this window, and the sidebar's.
    var visibleDocs: Set<DocID> {
        var out: Set<DocID> = []
        for path in [todayPath, libraryPath, todosPath, searchPath, booksPath, padPath] {
            for case .doc(let id) in path { out.insert(id) }
        }
        if case .doc(let id)? = padItem { out.insert(id) }
        return out
    }

    /// Do the navigation part of a command; the rest comes back as an effect.
    func perform(_ command: AppCommand, picker: WorkspacePicker) -> CommandEffect {
        switch command {
        case .newDoc:
            newDoc(in: nil)
        case .newFolder:
            newFolder(in: nil)
        case .search:
            showSearch()
            if isPad { searchFocusRequest += 1 }
        case .open:
            searchScopeRequest = .docs
            showSearch()
            searchFocusRequest += 1
        case .actions:
            showActions = true
        case .toggleSidebar:
            if isPad { sidebarHidden.toggle() }
        case .newDocHere:
            newDoc(in: focusedDoc)
        case .addBooks:
            showBooks()
            addBooksRequest += 1
        case .books:
            showBooks()
        case .review:
            showReview()
        case .back:
            goBack()
        case .forward:
            goForward()
        case .refresh:
            return .refresh
        case .findInDoc:
            findInDoc()
        case .shareLink:
            if let doc = focusedDoc { request(.shareLink, on: doc) }
        case .exportPDF:
            if let doc = focusedDoc { request(.exportPDF, on: doc) }
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

    /// Find in Doc: the doc in front opens its find bar (⌘F, the … menu).
    func findInDoc() {
        if let doc = focusedDoc { request(.find, on: doc) }
    }

    func request(_ kind: DocAction.Kind, on doc: DocID) {
        docAction = DocAction(doc: doc, kind: kind, serial: (docAction?.serial ?? 0) + 1)
    }

    /// A push about comments on a doc's link: show the doc, then its link comments.
    func openLinkComments(_ doc: DocID) {
        if focusedDoc != doc {
            if isPad { select(.doc(doc)) } else { tab = .library; libraryPath = [.doc(doc)] }
        }
        request(.linkComments, on: doc)
    }

    /// Development launch arguments, for screenshots and UI runs:
    /// `-tab today|library|todos|search`, `-openDoc <title>`,
    /// `-searchQuery <text>`, `-showSettings YES`, `-tab books`, `-readBook <id>`, `-clearBookFiles YES`
    /// (Debug UI runs: drop the downloaded EPUBs). (`-serverURL <url>` is read
    /// by `AppModel` straight from UserDefaults' argument domain.)
    func applyLaunchArguments(index: DocIndex) {
        let d = UserDefaults.standard
        switch d.string(forKey: "tab") {
        case "library": tab = .library; padItem = .today
        case "todos": tab = .todos; padItem = .todos
        case "search": tab = .search; padItem = .search
        case "books": tab = .books; padItem = .books
        case "review": showReview()
        default: break
        }
        #if DEBUG
        // a UI test's first open of a book downloads it (no EPUB from an earlier run)
        if d.bool(forKey: "clearBookFiles"), SignInRelay.configured != nil, let dirs = try? FileManager.default.contentsOfDirectory(at: BookStore.directory, includingPropertiesForKeys: nil) {
            for dir in dirs { try? FileManager.default.removeItem(at: dir.appending(path: "epubs", directoryHint: .isDirectory)) }
        }
        #endif
        if let q = d.string(forKey: "searchQuery") { searchQuery = q }
        if let book = d.string(forKey: "readBook") { readBook(book) }
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
            // a doc lives in the Library (not on whichever tab launched)
            if UserDefaults.standard.string(forKey: "tab") == nil { tab = .library }
            open(.doc(doc.id))
        }
        return true
    }
}
