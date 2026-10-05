import SwiftUI
import TaisceKit

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
                .foregroundStyle(Theme.accent)
            Text("Moving your data")
                .font(.title2.weight(.semibold))
                .foregroundStyle(Theme.text)
            Text(reason)
                .font(.body)
                .foregroundStyle(Theme.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)
            if !conflicts.isEmpty {
                Text("This Mac already has a copy here, and the old one has \(unsent).")
                    .font(.callout)
                    .foregroundStyle(Theme.text)
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
                .tint(Theme.accent)
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
        .tint(Theme.accent)
        .sheet(isPresented: $router.showSettings) { SettingsScreen() }
        .sheet(isPresented: $router.showNewDoc) {
            NewDocSheet(parent: router.newDocParent).environment(router)
        }
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

/// "This doc is no longer shared with you." A capsule at the top that goes
/// by itself; tap to dismiss. VoiceOver announces it.
struct AccessNoticeBanner: View {
    let text: String
    var onDismiss: () -> Void = {}

    var body: some View {
        Button(action: onDismiss) {
            Label(text, systemImage: "person.2.slash")
                .font(.footnote.weight(.medium))
                .foregroundStyle(Theme.text)
                .padding(.horizontal, 16)
                .frame(minHeight: Theme.minTarget)
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
        }
    }
}

/// iPhone: the native floating tab bar (Liquid Glass on iOS 26).
struct PhoneRoot: View {
    @Environment(Router.self) private var router

    var body: some View {
        @Bindable var router = router
        TabView(selection: $router.tab) {
            Tab("Today", systemImage: "house", value: AppTab.today) {
                NavigationStack(path: $router.todayPath) {
                    TodayScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            }
            Tab("Library", systemImage: "folder", value: AppTab.library) {
                NavigationStack(path: $router.libraryPath) {
                    LibraryScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            }
            Tab("To-dos", systemImage: "checkmark.circle", value: AppTab.todos) {
                NavigationStack(path: $router.todosPath) {
                    TodosScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            }
            Tab("Search", systemImage: "magnifyingglass", value: AppTab.search, role: .search) {
                NavigationStack(path: $router.searchPath) {
                    SearchScreen().navigationDestination(for: Route.self) { RouteView(route: $0) }
                }
            }
        }
        .tabBarMinimizeBehavior(.onScrollDown)
    }
}

/// iPad: sidebar (search, Today, To-dos, the Library tree) and the detail
/// at reading width.
struct PadRoot: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    var body: some View {
        @Bindable var router = router
        NavigationSplitView {
            PadSidebar().modifier(DocDeleteHost())
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
        case .search?: SearchScreen(showsField: false)
        case .doc(let id)?: DocScreen(docID: id).id(id)
        }
    }
}

struct PadSidebar: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.docDeletion) private var docDeletion
    @FocusState private var searchFocused: Bool
    /// folders opened by hand (the ones above the doc in front open by themselves)
    @State private var expanded: Set<DocID> = []

    /// What the List selects, and the folders that must be open for its row.
    private var sidebarFront: (selection: PadItem?, reveal: Set<DocID>) {
        let front = router.sidebarSelection
        guard case .doc(let id)? = front else { return (front, []) }
        guard let above = LibraryNode.ancestors(of: id, in: model.library) else { return (router.padItem, []) }
        return (front, Set(above))
    }

    var body: some View {
        @Bindable var router = router
        // The List must have a row for whatever it selects: told to select a
        // doc it has no row for (inside a closed folder, or outside this
        // workspace), SwiftUI empties the detail stack and the doc never
        // opens. So the folders above the doc in front stay open, and a doc
        // the tree doesn't hold leaves the selection where it was.
        let (shown, reveal) = sidebarFront
        let selection = Binding<PadItem?>(get: { shown }, set: { if let item = $0 { router.select(item) } })
        List(selection: selection) {
            if model.hasWorkspaces {
                SidebarWorkspaceRow()
                    .listRowBackground(Color.clear)
                    .listRowInsets(EdgeInsets(top: 0, leading: 8, bottom: 0, trailing: 8))
            }
            Section {
                HStack(spacing: 8) {
                    Image(systemName: "magnifyingglass").foregroundStyle(Theme.secondary).accessibilityHidden(true)
                    TextField("Search", text: $router.searchQuery)
                        .focused($searchFocused)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .submitLabel(.search)
                        .onSubmit { router.select(.search) }
                }
                .padding(.horizontal, 10)
                .frame(minHeight: 38)
                .background(Theme.surface2, in: .rect(cornerRadius: 10, style: .continuous))
                .listRowBackground(Color.clear)
                .listRowInsets(EdgeInsets(top: 4, leading: 12, bottom: 4, trailing: 12))
                .onChange(of: router.searchQuery) { _, q in
                    if !q.isEmpty, router.padItem != .search { router.select(.search) }
                }
                .onChange(of: router.searchFocusRequest) { searchFocused = true }
                // the tag goes last: a badge wrapping it hides it from the
                // List's selection on the Mac, so Today couldn't be clicked
                Label("Today", systemImage: "house")
                    .badge(model.dueCount)
                    .tag(PadItem.today)
                Label("To-dos", systemImage: "checkmark.circle").tag(PadItem.todos)
            }
            Section("Library") {
                SidebarTreeRows(nodes: model.library, expanded: $expanded, reveal: reveal, docDeletion: docDeletion)
            }
        }
        .listStyle(.sidebar)
        // what opened itself stays open after you move on
        .onChange(of: reveal) { _, r in expanded.formUnion(r) }
        .navigationTitle("Taisce")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                // ⌘N is the menu bar's (TaisceCommands)
                Button { router.newDoc() } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("New doc")
                    .help("New doc (⌘N)")
                    .disabled(!model.currentAccess.canCreate)
            }
        }
        .safeAreaInset(edge: .bottom) {
            HStack {
                SyncChip(badge: SyncBadge.make(status: model.syncStatus, pending: model.pendingWrites))
                Spacer()
                // no pull to refresh with a pointer
                Button { Task { try? await model.sync?.catchUp() } } label: {
                    Image(systemName: "arrow.clockwise").frame(width: Theme.minTarget, height: Theme.minTarget)
                }
                .accessibilityLabel("Refresh")
                .help("Refresh (⌘R)")
                Button { router.showSettings = true } label: {
                    Image(systemName: "gearshape").frame(width: Theme.minTarget, height: Theme.minTarget)
                }
                .accessibilityLabel("Settings")
                .help("Settings (⌘,)")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
        }
        .refreshable { try? await model.sync?.catchUp() }
    }
}

/// The sidebar's Library tree. A folder in `reveal` is open whatever
/// `expanded` says: closing it while its doc is in front would leave the
/// List selecting a row it doesn't have.
private struct SidebarTreeRows: View {
    let nodes: [LibraryNode]
    @Binding var expanded: Set<DocID>
    let reveal: Set<DocID>
    let docDeletion: DocDeletion?
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router

    var body: some View {
        ForEach(nodes) { node in
            if let kids = node.children {
                DisclosureGroup(isExpanded: open(node.id)) {
                    SidebarTreeRows(nodes: kids, expanded: $expanded, reveal: reveal, docDeletion: docDeletion)
                } label: {
                    row(node)
                }
            } else {
                row(node)
            }
        }
    }

    private func open(_ id: DocID) -> Binding<Bool> {
        Binding(
            get: { expanded.contains(id) || reveal.contains(id) },
            set: { if $0 { expanded.insert(id) } else { expanded.remove(id) } }
        )
    }

    private func row(_ node: LibraryNode) -> some View {
        Label(node.doc.title, systemImage: node.isFolder ? "folder" : "doc.text")
            .tag(PadItem.doc(node.id))
            .contextMenu {
                Button(model.isPinned(node.id) ? "Unpin from Today" : "Pin to Today",
                       systemImage: model.isPinned(node.id) ? "pin.slash" : "pin") { model.togglePin(node.id) }
                if model.access(for: node.id).canCreate {
                    Button("New doc here", systemImage: "plus") { router.newDoc(in: node.id) }
                }
                if let docDeletion, docDeletion.canDelete(node.id) {
                    Divider()
                    Button("Delete\u{2026}", systemImage: "trash", role: .destructive) { docDeletion.ask(node.id) }
                }
            }
    }
}

/// While discovery decides whether the server needs sign-in.
struct LaunchView: View {
    var body: some View {
        VStack(spacing: 16) {
            Text("Taisce")
                .font(.system(size: 40, weight: .semibold, design: .serif))
                .foregroundStyle(Theme.text)
            ProgressView().tint(Theme.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(Theme.ground.ignoresSafeArea())
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Taisce, connecting")
    }
}

#if DEBUG
#Preview("Launch") {
    LaunchView().preferredColorScheme(.dark)
}
#endif
