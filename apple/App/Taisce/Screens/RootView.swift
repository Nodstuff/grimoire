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
            if model.needsSignIn {
                SignInScreen()
            } else if sizeClass == .regular {
                PadRoot()
            } else {
                PhoneRoot()
            }
        }
        .environment(router)
        .tint(Theme.accent)
        .sheet(isPresented: $router.showSettings) { SettingsScreen() }
        .onChange(of: sizeClass, initial: true) { _, size in router.isPad = size == .regular }
        .onChange(of: model.treeLoaded) { _, loaded in
            guard loaded, !appliedLaunchArguments else { return }
            appliedLaunchArguments = true
            router.applyLaunchArguments(index: model.index)
        }
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
                Label("Today", systemImage: "house").tag(PadItem.today)
                Label("To-dos", systemImage: "checkmark.circle").tag(PadItem.todos)
            }
            Section("Library") {
                OutlineGroup(model.library, children: \.children) { node in
                    Label(node.doc.title, systemImage: node.isFolder ? "folder" : "doc.text")
                        .tag(PadItem.doc(node.id))
                        .contextMenu {
                            Button(model.isPinned(node.id) ? "Unpin from Today" : "Pin to Today",
                                   systemImage: model.isPinned(node.id) ? "pin.slash" : "pin") { model.togglePin(node.id) }
                        }
                }
            }
        }
        .listStyle(.sidebar)
        .navigationTitle("Taisce")
        .safeAreaInset(edge: .bottom) {
            HStack {
                SyncChip(badge: SyncBadge.make(status: model.syncStatus, pending: model.pendingWrites))
                Spacer()
                Button { router.showSettings = true } label: {
                    Image(systemName: "gearshape").frame(width: Theme.minTarget, height: Theme.minTarget)
                }
                .accessibilityLabel("Settings")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
        }
        .refreshable { try? await model.sync?.catchUp() }
    }
}
