import SwiftUI
import TaisceKit
import UIKit

/// The Mac's data move didn't finish: nothing opens until a launch from
/// Finder (which may read the old container) completes it.
struct MigrationBlockedView: View {
    let reason: String
    var conflicts: [SandboxMigration.Conflict] = []
    var resolve: (Bool) -> Void = { _ in }

    var unsent: String {
        let known = conflicts.compactMap(\.unsent)
        guard known.count == conflicts.count else { return "an unknown number of unsent changes" }
        let n = known.reduce(0, +)
        return n == 1 ? "1 unsent change" : "\(n) unsent changes"
    }

    var body: some View {
        VStack(spacing: 16) {
            Image(systemName: "shippingbox")
                .font(.system(size: 40))
                .foregroundStyle(Color.lapis.accent)
            Text("Moving your data")
                .font(.title2.weight(.semibold))
                .foregroundStyle(Color.lapis.ink)
            Text(reason)
                .font(.body)
                .foregroundStyle(Color.lapis.ink3)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
            if !conflicts.isEmpty {
                Text("This Mac already has a copy here, and the old one has \(unsent).")
                    .font(.callout)
                    .foregroundStyle(Color.lapis.ink)
                    .multilineTextAlignment(.center)
                    .frame(maxWidth: 420)
                Button("Use the copy already here (the old one stays untouched)") { resolve(true) }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("migration.keepHere")
                Button("Use the old copy, with its unsent changes (this one is set aside)") { resolve(false) }
                    .buttonStyle(.bordered)
                    .accessibilityIdentifier("migration.useOld")
            }
            Button("Quit Taisce") { exit(0) }
                .buttonStyle(.borderedProminent)
                .tint(Color.lapis.accent)
                .accessibilityIdentifier("migration.quit")
        }
        .padding(40)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .groundBackground()
        .accessibilityIdentifier("migration.blocked")
    }
}

/// Sign-in, or the phone tabs / pad split view by size class.
struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var router = Router()
    @State private var appliedLaunchArguments = false
    @State private var window = WindowHolder()
    @Environment(\.scenePhase) private var scenePhase

    var body: some View {
        @Bindable var router = router
        Group {
            if let reason = model.migrationBlocked {
                MigrationBlockedView(reason: reason, conflicts: model.migrationConflicts) { keepHere in
                    Task { await model.resolveMigration(keepHere: keepHere) }
                }
            } else if model.isCheckingAuth {
                LaunchView()
            } else if model.needsSignIn {
                SignInScreen()
            } else if RootLayout.make(horizontal: sizeClass) == .split {
                PadRoot()
            } else {
                PhoneRoot()
            }
        }
        .environment(router)
        // book covers sit behind the server's bearer (Today, Search, Books)
        .environment(\.coverImageLoader, model.books.coverLoader)
        .tint(Color.lapis.accent)
        .sheet(isPresented: $router.showSettings) { SettingsScreen() }
        .sheet(isPresented: $router.showActions) { ActionsPalette().environment(model).environment(router) }
        .sheet(isPresented: $router.showNewDoc) {
            NewDocSheet(parent: router.newDocParent).environment(router)
        }
        .modifier(NewFolderPrompt(router: router, model: model))
        // the cover's own hosting on the Mac doesn't inherit the window's model: hand it over
        .fullScreenCover(item: $router.readingBook) { BookReaderScreen(bookID: $0.id).environment(model).environment(router) }
        #if DEBUG
        .modifier(SignInRelayAutoStart())
        #endif
        .onChange(of: sizeClass, initial: true) { _, size in router.isPad = RootLayout.make(horizontal: size) == .split }
        // the menu bar's commands act on the window in front
        .focusedSceneValue(\.router, router)
        #if targetEnvironment(macCatalyst)
        .background(MacWindowSetup())
        #endif
        .onChange(of: model.treeLoaded) { _, loaded in
            guard loaded, !appliedLaunchArguments else { return }
            appliedLaunchArguments = true
            router.applyLaunchArguments(index: model.index)
        }
        .onChange(of: model.docs.count) {
            if appliedLaunchArguments { _ = router.openLaunchDoc(index: model.index) }
        }
        // a tap on "X commented on Y": the frontmost window shows that doc's link comments
        .background(WindowReader(holder: window))
        .onChange(of: model.linkCommentsRequest, initial: true) { openLinkCommentsIfFront() }
        .onChange(of: scenePhase) { openLinkCommentsIfFront() }
        .onChange(of: model.treeLoaded) { openLinkCommentsIfFront() }
        // sign-out or another account: nothing queued for the last one survives
        .onChange(of: model.authPhase) { router.docAction = nil }
        // ADR 0004: an unshared doc closes, with a gentle note
        .onChange(of: model.revocation) { _, r in
            guard let r, router.drop(r.docs) else { return }
            model.accessNotice = AppModel.revokedNotice
        }
        .overlay(alignment: .top) {
            if let note = model.accessNotice {
                AccessNoticeBanner(text: note) { model.accessNotice = nil }
                    .padding(.top, 8)
                    .transition(.move(edge: .top).combined(with: .opacity))
                    .task(id: note) {
                        try? await Task.sleep(for: .seconds(5))
                        if model.accessNotice == note { model.accessNotice = nil }
                    }
            }
        }
        .animation(.default, value: model.accessNotice)
    }
}

extension RootView {
    /// Takes a waiting comment-push request if this window is the one in
    /// front: the key window, or, with none key (the tap just launched or
    /// woke the app), the first active one.
    func openLinkCommentsIfFront() {
        guard model.linkCommentsRequest != nil, model.treeLoaded,
              FrontWindow.isFront(window.window, scenePhase: scenePhase)
        else { return }
        guard let doc = model.takeLinkCommentsRequest() else { return }
        router.openLinkComments(doc)
        Task { try? await model.shareLinks.load(doc: doc) }
    }
}

/// Which window takes a request meant for "the one in front".
enum FrontWindow {
    @MainActor static func isFront(_ window: UIWindow?, scenePhase: ScenePhase) -> Bool {
        guard scenePhase == .active, let window else { return false }
        if window.isKeyWindow { return true }
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let anyKey = scenes.contains { $0.windows.contains(where: \.isKeyWindow) }
        guard !anyKey else { return false }
        return scenes.first { $0.activationState == .foregroundActive } === window.windowScene
    }
}

/// The UIWindow a SwiftUI view sits in.
@MainActor final class WindowHolder {
    weak var window: UIWindow?
}

struct WindowReader: UIViewRepresentable {
    let holder: WindowHolder

    func makeUIView(context: Context) -> Probe { Probe(holder: holder) }
    func updateUIView(_ view: Probe, context: Context) {}

    final class Probe: UIView {
        let holder: WindowHolder
        init(holder: WindowHolder) {
            self.holder = holder
            super.init(frame: .zero)
            isUserInteractionEnabled = false
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError() }

        override func didMoveToWindow() {
            super.didMoveToWindow()
            holder.window = window
        }
    }
}

/// "This doc is no longer shared with you." A capsule at the top that goes
/// by itself; tap to dismiss. VoiceOver announces it.
struct AccessNoticeBanner: View {
    let text: String
    var onDismiss: () -> Void = {}

    var body: some View {
        Button(action: onDismiss) {
            Label(text, systemImage: "person.2.slash")
                .font(.footnote.weight(.medium))
                .foregroundStyle(Color.lapis.ink)
                .padding(.horizontal, 16)
                .frame(minHeight: Lapis.Layout.minTarget)
                .glassEffect(.regular, in: .capsule)
        }
        .buttonStyle(.plain)
        .accessibilityHint("Dismisses this note")
        .accessibilityIdentifier("access.notice")
        .onAppear { AccessibilityNotification.Announcement(text).post() }
    }
}

/// Which root a window shows. Regular width (iPad, every Mac window) gets
/// the split view; compact width (iPhone, iPad slide-over) the tab bar.
enum RootLayout: Equatable {
    case tabs, split

    static func make(horizontal: UserInterfaceSizeClass?) -> RootLayout {
        horizontal == .regular ? .split : .tabs
    }
}

/// Where a pushed route goes.
struct RouteView: View {
    let route: Route

    var body: some View {
        switch route {
        case .doc(let id): DocScreen(docID: id).id(id)
        case .todos: TodosScreen()
        case .review: ReviewScreen()
        case .book(let id): BookDetail(id: id).id(id)
        case .bookReader(let id): BookReaderScreen(bookID: id).id(id)
        }
    }
}

/// iPhone: the native floating tab bar (Liquid Glass on iOS 26): Today,
/// Library, Books, To-dos, and Search on its own. Today's badge counts the
/// changes waiting for review (its card opens the list).
struct PhoneRoot: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    var body: some View {
        @Bindable var router = router
        TabView(selection: $router.tab) {
            Tab(value: AppTab.today) {
                NavigationStack(path: $router.todayPath) {
                    TodayScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            } label: { TabLabel("Today", "house") }
            .badge(model.home.summary.total)
            Tab(value: AppTab.library) {
                NavigationStack(path: $router.libraryPath) {
                    LibraryScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            } label: { TabLabel("Library", "folder") }
            Tab(value: AppTab.books) {
                NavigationStack(path: $router.booksPath) {
                    BooksScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            } label: { TabLabel("Books", "books.vertical") }
            Tab(value: AppTab.todos) {
                NavigationStack(path: $router.todosPath) {
                    TodosScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            } label: { TabLabel("To-dos", "checkmark.circle") }
            Tab(value: AppTab.search, role: .search) {
                NavigationStack(path: $router.searchPath) {
                    SearchScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            } label: { TabLabel("Search", "magnifyingglass") }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
        // Today's badge counts the review queue whichever tab opens first
        .task(id: model.cursor) {
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await model.home.loadReview()
        }
    }
}

/// A tab's label with its outlined symbol (the artboards' tab bar; the
/// system fills tab symbols by default).
private struct TabLabel: View {
    let title: String
    let symbol: String
    init(_ title: String, _ symbol: String) {
        self.title = title
        self.symbol = symbol
    }

    var body: some View {
        Label { Text(title) } icon: { Image(systemName: symbol).environment(\.symbolVariants, .none) }
    }
}

/// iPad: sidebar (search, Today, To-dos, the Library tree) and the detail
/// at reading width.
struct PadRoot: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    var body: some View {
        @Bindable var router = router
        NavigationSplitView(columnVisibility: $router.columnVisibility) {
            PadSidebar().modifier(DocDeleteHost()).modifier(DocMoveHost())
        } detail: {
            NavigationStack(path: $router.padPath) {
                detail.navigationDestination(for: Route.self) { RouteView(route: $0) }
            }
        }
        .navigationSplitViewStyle(.balanced)
        // the sidebar carries the switcher; screens drop their bar
        .environment(\.sidebarCarriesWorkspaces, true)
    }

    @ViewBuilder private var detail: some View {
        switch router.padItem {
        case .today?, nil: TodayScreen()
        case .todos?: TodosScreen()
        case .review?: ReviewScreen()
        case .books?: BooksScreen(filter: Bindable(router).bookShelf)
        case .search?: SearchScreen()
        case .doc(let id)?: DocScreen(docID: id).id(id)
        }
    }
}

struct PadSidebar: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.docDeletion) private var docDeletion
    @Environment(\.docMover) private var docMover
    /// folders opened by hand (the ones above the doc in front open by themselves)
    @State private var expanded: Set<DocID> = []
    @State private var scroller = TreeScroller()
    /// the selection changed by a click in the tree (already in view: no scroll)
    @State private var fromTree = false
    @State private var creatingWorkspace = false
    @State private var managingWorkspaces = false

    /// The doc in front, and the folders above it (open, so its row shows).
    private var front: (selection: PadItem?, reveal: Set<DocID>) {
        let front = router.sidebarSelection
        guard case .doc(let id)? = front, let above = LibraryNode.ancestors(of: id, in: model.library) else { return (front, []) }
        return (front, Set(above))
    }

    var body: some View {
        let (shown, reveal) = front
        let selectedDoc: DocID? = if case .doc(let id)? = shown { id } else { nil }
        // Not a List: List rows never receive drops (iOS and the Mac), so
        // the Library is a LibraryTree in a plain scroll view.
        ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    // Today · To-dos · Review · Books · Search (Mac-Main, iPad-Library-Doc).
                    // To-dos counts what the To-dos screen lists; Review what waits for you.
                    SidebarItem(title: "Today", systemImage: "house", selected: shown == .today) { router.select(.today) }
                    SidebarItem(title: "To-dos", systemImage: "checkmark.circle", badge: model.openTodoCount,
                                badgeLabel: "open", selected: shown == .todos) { router.select(.todos) }
                    SidebarItem(title: "Review", systemImage: "flag", badge: model.home.summary.total, badgeTone: .attention,
                                badgeLabel: "to review", selected: shown == .review) { router.select(.review) }
                    SidebarItem(title: "Books", systemImage: "books.vertical", badge: model.books.shelf.books.count,
                                badgeLabel: "books", selected: shown == .books) {
                        router.bookShelf = .all
                        router.select(.books)
                    }
                    if shown == .books, model.books.availability == .available, !model.books.shelf.books.isEmpty {
                        BooksSidebarSections()
                    }
                    SidebarItem(title: "Search", systemImage: "magnifyingglass", shortcut: "\u{21E7}\u{2318}F", selected: shown == .search) {
                        router.select(.search)
                        router.focusSearch()
                    }
                    // every workspace its own section, the current one open with its
                    // tree (Mac-Main, iPad-Library-Doc); a header switches to it (⌘1…⌘9)
                    if model.hasWorkspaces {
                        let picker = model.workspacePicker
                        ForEach(Array(picker.options.enumerated()), id: \.element) { i, scope in
                            let current = scope == picker.current
                            WorkspaceSectionHeader(scope: scope, picker: picker, number: i + 1, current: current,
                                                   canCreate: current && model.currentAccess.canCreate,
                                                   onSelect: { model.selectWorkspace(scope) },
                                                   onNewFolder: { router.newFolder(in: nil) },
                                                   onNewWorkspace: { creatingWorkspace = true },
                                                   onManage: { managingWorkspaces = true })
                                .modifier(TopLevelDropIf(on: current, mover: docMover))
                            if current {
                        LibraryTree(nodes: model.library, expanded: $expanded, reveal: reveal, selected: selectedDoc,
                                pins: Set(model.pins), style: .sidebar, onOpen: { fromTree = true; router.select(.doc($0)) }) { node in
                            rowMenu(node)
                        }
                            }
                        }
                    } else {
                        HStack {
                            Text("Library".uppercased())
                                .lapisFont(.label)
                                .foregroundStyle(Color.lapis.ink3)
                                .accessibilityLabel("Library")
                                .accessibilityAddTraits(.isHeader)
                            Spacer()
                            if model.currentAccess.canCreate {
                                Button { router.newFolder(in: nil) } label: {
                                    Image(systemName: "folder.badge.plus").foregroundStyle(Color.lapis.ink3)
                                        .frame(width: 30, height: 30).contentShape(.rect)
                                }
                                .buttonStyle(.plain)
                                .accessibilityLabel("New folder")
                                .accessibilityIdentifier("tree.newFolder")
                                .help("New folder (⇧⌘N)")
                            }
                        }
                        .frame(maxWidth: .infinity, minHeight: 30, alignment: .leading)
                        .padding(.top, 12)
                        .padding(.leading, 6)
                        .contentShape(.rect)
                        .docTopLevelDrop(mover: docMover)
                    LibraryTree(nodes: model.library, expanded: $expanded, reveal: reveal, selected: selectedDoc,
                            pins: Set(model.pins), style: .sidebar, onOpen: { fromTree = true; router.select(.doc($0)) }) { node in
                        rowMenu(node)
                    }
                    }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
        }
        .scrollDismissesKeyboard(.immediately)
        // drags near the top or bottom edge scroll it
        .treeScrolling(scroller)
        // the doc in front comes into view (a link followed, a search hit)
        .onChange(of: selectedDoc) { _, id in
            defer { fromTree = false }
            guard let id, !fromTree else { return }
            withAnimation { scroller.position.scrollTo(id: id, anchor: .center) }
        }
        // what opened itself stays open after you move on
        .onChange(of: reveal) { _, r in expanded.formUnion(r) }
        // a folder just made: the folders above it open, the tree scrolls to it
        .onChange(of: model.docs.count) { revealIfReady() }
        .onChange(of: router.revealDoc) { revealIfReady() }
        // the sidebar's own ground (Lapis panel), not the system material
        .scrollContentBackground(.hidden)
        .background(Color.lapis.panel.ignoresSafeArea())
        // the review count and the sidebar's To-dos count stay current
        .task(id: model.cursor) {
            try? await Task.sleep(for: .milliseconds(400))
            guard !Task.isCancelled else { return }
            await model.home.loadReview()
        }
        .task(id: model.todoRevision) { _ = await model.loadTodos() }
        // the Books count (and the shelves under it) from the cached shelf, then the server's
        .task(id: model.authPhase) { await model.books.load() }
        .animation(Lapis.Motion.standard, value: shown == .books)
        .navigationTitle("Taisce")
        .toolbar {
            #if !targetEnvironment(macCatalyst)
            // iPad-Library-Doc: the logo on the panel; the Mac's sidebar starts
            // with its traffic lights alone (Mac-Main)
            ToolbarItem(placement: .topBarLeading) { SidebarBrand() }.sharedBackgroundVisibility(.hidden)
            #endif
            ToolbarItem(placement: .principal) { Color.clear.frame(width: 1, height: 1).accessibilityHidden(true) }
        }
        .safeAreaInset(edge: .bottom) {
            // the avatar and the sync state (Mac-Main); Settings and Refresh in
            // the avatar's menu, and on ⌘, / ⌘R
            SidebarAccountFooter()
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                // the tree scrolls under it: a bar, not text over rows
                .background(Color.lapis.panel)
                .overlay(alignment: .top) { Rectangle().fill(Color.lapis.line).frame(height: 1).padding(.horizontal, 10) }
        }
        .sheet(isPresented: $creatingWorkspace) { NewWorkspaceSheet().environment(model) }
        .sheet(isPresented: $managingWorkspaces) { ManageWorkspacesSheet().environment(model) }
        .refreshable { try? await model.sync?.catchUp() }
    }

    private func revealIfReady() {
        guard let id = router.revealDoc, let above = LibraryNode.ancestors(of: id, in: model.library) else { return }
        router.revealDoc = nil
        withAnimation(.snappy(duration: 0.2)) { expanded.formUnion(above) }
        Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(150))
            withAnimation { scroller.position.scrollTo(id: id, anchor: .center) }
        }
    }

    @ViewBuilder private func rowMenu(_ node: LibraryNode) -> some View {
        Button(model.isPinned(node.id) ? "Unpin from Today" : "Pin to Today",
               systemImage: model.isPinned(node.id) ? "pin.slash" : "pin") { model.togglePin(node.id) }
        if model.access(for: node.id).canCreate {
            Button("New doc here", systemImage: "plus") { router.newDoc(in: node.id) }
            Button("New folder here", systemImage: "folder.badge.plus") { router.newFolder(in: node.id) }
        }
        if let docMover, docMover.canMove(node.id) {
            Button("Move to\u{2026}", systemImage: "folder") { docMover.ask(node.id) }
        }
        if let docDeletion, docDeletion.canDelete(node.id) {
            Divider()
            Button("Delete\u{2026}", systemImage: "trash", role: .destructive) { docDeletion.ask(node.id) }
        }
    }
}

/// "New folder": its name, then an empty doc made where asked, not opened
/// (a folder until something is written in it; drop docs into it).
struct NewFolderPrompt: ViewModifier {
    let router: Router
    let model: AppModel
    @State private var name = ""
    @State private var failure: String?

    func body(content: Content) -> some View {
        @Bindable var router = router
        content
            .alert("New folder", isPresented: $router.showNewFolder) {
                TextField("Name", text: $name)
                    .accessibilityIdentifier("newfolder.name")
                Button("Cancel", role: .cancel) { name = "" }
                Button("Create") { create() }
                    .accessibilityIdentifier("newfolder.create")
            } message: {
                Text(router.newFolderParent.flatMap { model.index.byID[$0]?.title }.map { "Inside \u{201C}\($0)\u{201D}" } ?? "At the top level")
            }
            .alert("Couldn't make the folder", isPresented: Binding(get: { failure != nil }, set: { if !$0 { failure = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(failure ?? "")
            }
    }

    private func create() {
        let title = name.trimmingCharacters(in: .whitespacesAndNewlines)
        name = ""
        let parent = router.newFolderParent
        Task { @MainActor in
            do {
                let id = try await model.createDoc(title: title.isEmpty ? "New folder" : title, parent: parent, open: false)
                router.revealDoc = id
            } catch {
                failure = error.localizedDescription
            }
        }
    }
}

/// A sidebar destination (Today, To-dos, Review, Books, Search): an icon,
/// the name, and a count (Review's in the attention yellow) or, on a
/// keyboard, its shortcut.
private struct SidebarItem: View {
    enum BadgeTone { case quiet, attention }

    let title: String
    let systemImage: String
    var badge = 0
    var badgeTone: BadgeTone = .quiet
    /// what the count counts, for VoiceOver ("6 open")
    var badgeLabel: String?
    /// shown with a pointer (Mac, iPad with a trackpad): "⇧⌘F"
    var shortcut: String?
    let selected: Bool
    let action: () -> Void
    @State private var pointer = false

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: systemImage)
                .font(.body)
                .foregroundStyle(badgeTone == .attention ? Color.lapis.yellow : Color.lapis.accent)
                .frame(width: 24)
                .accessibilityHidden(true)
            Text(title).foregroundStyle(Color.lapis.ink)
            Spacer(minLength: 0)
            if badge > 0 {
                Text("\(badge)")
                    .font(.subheadline.weight(badgeTone == .attention ? .semibold : .regular))
                    .monospacedDigit()
                    .foregroundStyle(badgeTone == .attention ? Color.lapis.yellow : Color.lapis.ink3)
            } else if let shortcut {
                Text(shortcut).font(.caption).foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
            }
        }
        .padding(.horizontal, 10)
        .frame(maxWidth: .infinity, minHeight: Lapis.Layout.sidebarRow, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: Lapis.Radius.field, style: .continuous)
            .fill(selected ? Color.lapis.accentSoft : pointer ? Color.lapis.hover : .clear))
        .contentShape(.rect)
        .onHover { pointer = $0 }
        .onTapGesture(perform: action)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(badge > 0 ? "\(title), \(badge)\(badgeLabel.map { " \($0)" } ?? "")" : title)
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction { action() }
        .accessibilityIdentifier("sidebar.\(title.lowercased())")
    }
}

/// The current workspace's color, or the accent.
private func sectionColor(_ picker: WorkspacePicker, _ scope: WorkspaceScope) -> Color {
    Color(workspaceColor: picker.color(scope)) ?? Color.lapis.accent
}

/// A workspace's section header in the sidebar: "■ WORK  ⌘1" (the current
/// one, its tree below), "HOME  ⌘2", "FAMILY · SHARED  ⌘3". A click
/// switches; its menu adds a folder or a workspace, or manages them.
struct WorkspaceSectionHeader: View {
    let scope: WorkspaceScope
    let picker: WorkspacePicker
    let number: Int
    let current: Bool
    let canCreate: Bool
    let onSelect: () -> Void
    let onNewFolder: () -> Void
    let onNewWorkspace: () -> Void
    let onManage: () -> Void
    @State private var pointer = false

    private var title: String {
        picker.name(scope).uppercased() + (picker.isShared(scope) ? " \u{00B7} SHARED" : "")
    }

    var body: some View {
        HStack(spacing: 8) {
            if current {
                RoundedRectangle(cornerRadius: 2.5, style: .continuous).fill(sectionColor(picker, scope))
                    .frame(width: 9, height: 9).accessibilityHidden(true)
            }
            Text(title)
                .lapisFont(.label)
                .foregroundStyle(current ? Color.lapis.ink2 : Color.lapis.ink3)
                .lineLimit(1)
            Spacer(minLength: 4)
            if current, canCreate, pointer {
                Button(action: onNewFolder) {
                    Image(systemName: "folder.badge.plus").font(.footnote).foregroundStyle(Color.lapis.ink3)
                        .frame(width: 24, height: 24).contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("New folder")
                .help("New folder (⇧⌘N)")
            }
            if number <= 9 {
                Text("\u{2318}\(number)").font(.caption).foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
            }
        }
        .padding(.leading, 10)
        .padding(.trailing, 10)
        .padding(.top, 14)
        .frame(maxWidth: .infinity, minHeight: Lapis.Layout.sidebarRow, alignment: .leading)
        .contentShape(.rect)
        .onHover { pointer = $0 }
        .onTapGesture(perform: onSelect)
        .contextMenu {
            if canCreate { Button("New folder", systemImage: "folder.badge.plus", action: onNewFolder) }
            Button("New workspace\u{2026}", systemImage: "plus", action: onNewWorkspace)
            Button("Manage workspaces", systemImage: "slider.horizontal.3", action: onManage)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(picker.accessibilityName(scope))
        .accessibilityValue(current ? "current workspace" : "")
        .accessibilityHint(current ? "" : "Switches to this workspace")
        .accessibilityAddTraits(current ? [.isHeader, .isSelected, .isButton] : [.isHeader, .isButton])
        .accessibilityAction { onSelect() }
        .accessibilityAction(named: "New workspace", onNewWorkspace)
        .accessibilityAction(named: "Manage workspaces", onManage)
        .accessibilityIdentifier("sidebar.workspace.\(picker.name(scope))")
    }
}

/// A doc dropped on the current workspace's header goes to its top level.
struct TopLevelDropIf: ViewModifier {
    let on: Bool
    let mover: DocMover?

    func body(content: Content) -> some View {
        if on { content.docTopLevelDrop(mover: mover) } else { content }
    }
}

/// The foot of the sidebar: your initial and the sync state with the server
/// ("● Synced · taisce.null.ie"). A click opens the menu: Settings,
/// Refresh, Manage workspaces.
private struct SidebarAccountFooter: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    private var badge: SyncBadge { SyncBadge.make(status: model.syncStatus, pending: model.pendingWrites) }
    private var host: String { URL(string: model.serverURL)?.host() ?? model.serverURL }

    var body: some View {
        Menu {
            Button("Settings\u{2026}", systemImage: "gearshape") { router.showSettings = true }
            Button("Refresh", systemImage: "arrow.clockwise") { Task { try? await model.sync?.catchUp() } }
        } label: {
            HStack(spacing: 10) {
                LapisAvatar(name: model.home.profileName ?? "Taisce", size: 26)
                SyncDotLabel(badge: badge)
                Text("\u{00B7} \(host)").font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 0)
            }
            .frame(minHeight: Lapis.Layout.minTarget)
            .contentShape(.rect)
        }
        .menuStyle(.button)
        .buttonStyle(.plain)
        .accessibilityLabel("Account, \(SyncDotLabel(badge: badge).text), \(host)")
        .accessibilityHint("Settings and refresh")
        .accessibilityIdentifier("sidebar.account")
        .help("Settings (⌘,)")
        .task { await model.home.loadProfile() }
    }
}

/// "◆ Taisce" at the top of the sidebar.
private struct SidebarBrand: View {
    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: "diamond.inset.filled")
                .font(.title3.weight(.semibold))
                .foregroundStyle(Color.lapis.accent)
                .accessibilityHidden(true)
            Text("Taisce")
                .lapisFont(.docHeading2, weight: .medium)
                .foregroundStyle(Color.lapis.ink)
        }
        .fixedSize()
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isHeader)
    }
}

/// While discovery decides whether the server needs sign-in.
struct LaunchView: View {
    var body: some View {
        VStack(spacing: 16) {
            Text("Taisce")
                .font(.newsreader(40, weight: .medium))
                .tracking(-0.8)
                .foregroundStyle(Color.lapis.ink)
            ProgressView().tint(Color.lapis.ink3)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Color.lapis.bg.ignoresSafeArea())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Taisce, connecting")
    }
}

#if DEBUG
#Preview("Launch") {
    LaunchView().preferredColorScheme(.dark)
}
#endif
