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
        .tint(Color.lapis.accent)
        .sheet(isPresented: $router.showSettings) { SettingsScreen() }
        .sheet(isPresented: $router.showNewDoc) {
            NewDocSheet(parent: router.newDocParent).environment(router)
        }
        .modifier(NewFolderPrompt(router: router, model: model))
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
        case .search?: SearchScreen(showsField: false)
        case .doc(let id)?: DocScreen(docID: id).id(id)
        }
    }
}

struct PadSidebar: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.docDeletion) private var docDeletion
    @Environment(\.docMover) private var docMover
    @FocusState private var searchFocused: Bool
    /// folders opened by hand (the ones above the doc in front open by themselves)
    @State private var expanded: Set<DocID> = []
    @State private var scroller = TreeScroller()
    /// the selection changed by a click in the tree (already in view: no scroll)
    @State private var fromTree = false

    /// The doc in front, and the folders above it (open, so its row shows).
    private var front: (selection: PadItem?, reveal: Set<DocID>) {
        let front = router.sidebarSelection
        guard case .doc(let id)? = front, let above = LibraryNode.ancestors(of: id, in: model.library) else { return (front, []) }
        return (front, Set(above))
    }

    var body: some View {
        @Bindable var router = router
        let (shown, reveal) = front
        let selectedDoc: DocID? = if case .doc(let id)? = shown { id } else { nil }
        // Not a List: List rows never receive drops (iOS and the Mac), so
        // the Library is a LibraryTree in a plain scroll view.
        ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    if model.hasWorkspaces {
                        SidebarWorkspaceRow().padding(.horizontal, 2)
                    }
                    HStack(spacing: 8) {
                        Image(systemName: "magnifyingglass").foregroundStyle(Color.lapis.ink3).accessibilityHidden(true)
                        TextField("Search", text: $router.searchQuery)
                            .focused($searchFocused)
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .submitLabel(.search)
                            .onSubmit { router.select(.search) }
                    }
                    .padding(.horizontal, 10)
                    .frame(minHeight: 34)
                    .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.field, style: .continuous).strokeBorder(Color.lapis.line2, lineWidth: 1))
                    .padding(.vertical, 6)
                    .onChange(of: router.searchQuery) { _, q in
                        if !q.isEmpty, router.padItem != .search { router.select(.search) }
                    }
                    .onChange(of: router.searchFocusRequest) { searchFocused = true }
                    SidebarItem(title: "Today", systemImage: "house", badge: model.dueCount, selected: shown == .today) { router.select(.today) }
                    SidebarItem(title: "To-dos", systemImage: "checkmark.circle", selected: shown == .todos) { router.select(.todos) }
                    // drop a dragged doc on the header: the top level
                    HStack {
                        Text("Library")
                            .font(.subheadline.weight(.semibold))
                            .foregroundStyle(Color.lapis.ink3)
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
                    Image(systemName: "arrow.clockwise").frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                }
                .accessibilityLabel("Refresh")
                .help("Refresh (⌘R)")
                Button { router.showSettings = true } label: {
                    Image(systemName: "gearshape").frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                }
                .accessibilityLabel("Settings")
                .help("Settings (⌘,)")
            }
            .padding(.horizontal, 16)
            .padding(.vertical, 6)
            // the tree scrolls under it: a bar, not text over rows
            .background(.bar)
        }
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

/// Today and To-dos in the sidebar: a row that selects its screen.
private struct SidebarItem: View {
    let title: String
    let systemImage: String
    var badge = 0
    let selected: Bool
    let action: () -> Void
    @State private var pointer = false

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: systemImage).foregroundStyle(Color.lapis.accent).frame(width: 22).accessibilityHidden(true)
            Text(title).foregroundStyle(Color.lapis.ink)
            Spacer(minLength: 0)
            if badge > 0 {
                Text("\(badge)").font(.caption.weight(.semibold)).foregroundStyle(Color.lapis.ink3)
            }
        }
        .padding(.horizontal, 8)
        .frame(maxWidth: .infinity, minHeight: 30, alignment: .leading)
        .background(RoundedRectangle(cornerRadius: 7, style: .continuous)
            .fill(selected ? Color.lapis.accentSoft : pointer ? Color.lapis.hover : .clear))
        .contentShape(.rect)
        .onHover { pointer = $0 }
        .onTapGesture(perform: action)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(badge > 0 ? "\(title), \(badge)" : title)
        .accessibilityAddTraits(selected ? [.isButton, .isSelected] : .isButton)
        .accessibilityAction { action() }
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
