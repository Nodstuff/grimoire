import SwiftUI
import TaisceKit

/// Sign-in, or the phone tabs / pad split view by size class.
struct RootView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.horizontalSizeClass) private var sizeClass
    @State private var router = Router()
    @State private var appliedLaunchArguments = false

    var body: some View {
        @Bindable var router = router
        Group {
            if model.isCheckingAuth {
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
            PadSidebar()
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
    @FocusState private var searchFocused: Bool

    var body: some View {
        @Bindable var router = router
        let selection = Binding<PadItem?>(get: { router.padItem }, set: { if let item = $0 { router.select(item) } })
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
                Label("Today", systemImage: "house").tag(PadItem.today)
                    .badge(model.dueCount)
                Label("To-dos", systemImage: "checkmark.circle").tag(PadItem.todos)
            }
            Section("Library") {
                OutlineGroup(model.library, children: \.children) { node in
                    Label(node.doc.title, systemImage: node.isFolder ? "folder" : "doc.text")
                        .tag(PadItem.doc(node.id))
                        .contextMenu {
                            Button(model.isPinned(node.id) ? "Unpin from Today" : "Pin to Today",
                                   systemImage: model.isPinned(node.id) ? "pin.slash" : "pin") { model.togglePin(node.id) }
                            Button("New doc here", systemImage: "plus") { router.newDoc(in: node.id) }
                        }
                }
            }
        }
        .listStyle(.sidebar)
        .navigationTitle("Taisce")
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                // ⌘N is the menu bar's (TaisceCommands)
                Button { router.newDoc() } label: { Image(systemName: "square.and.pencil") }
                    .accessibilityLabel("New doc")
                    .help("New doc (⌘N)")
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
