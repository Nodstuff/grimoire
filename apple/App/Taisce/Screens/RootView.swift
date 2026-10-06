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
        let selection = Binding<PadItem?>(get: { router.sidebarSelection }, set: { if let item = $0 { router.select(item) } })
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
                OutlineGroup(model.library, children: \.children) { node in
                    Label(node.doc.title, systemImage: node.isFolder ? "folder" : "doc.text")
                        .tag(PadItem.doc(node.id))
                        .contextMenu {
                            Button(model.isPinned(node.id) ? "Unpin from Today" : "Pin to Today",
                                   systemImage: model.isPinned(node.id) ? "pin.slash" : "pin") { model.togglePin(node.id) }
                            if model.access(for: node.id).canCreate {
                                Button("New doc here", systemImage: "plus") { router.newDoc(in: node.id) }
                            }
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
