import SwiftUI
import TaisceKit

/// Agents: the fleet of Claude sessions the person's runtimes report (Agent
/// Fabric), by how much each needs them. A row opens its session; Talk
/// shows the agents' conversations; Hold all stops every Mac taking
/// anything but its release; a swipe snoozes a session's notifications,
/// one the other way pins it to the top.
///
/// The list holds still while it is on screen: a change redraws a row where
/// it stands. The server's order comes back on a pull to refresh and each
/// time the screen comes to the front.
struct AgentsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.scenePhase) private var scenePhase
    @State private var appeared = false
    @State private var presence = FleetScreenPresence()
    @State private var toast: ToastMessage?
    @State private var confirmHold = false
    @AppStorage(FleetPins.key) private var pins = ""

    private var store: FleetStore { model.fleet }
    private var inFront: Bool { appeared && scenePhase == .active }

    var body: some View {
        @Bindable var router = router
        // "5 min" moves on while the screen is open
        TimelineView(.periodic(from: .now, by: 30)) { context in
            AgentsContent(
                availability: store.availability,
                board: FleetBoard(store.fleet, now: context.date, pins: FleetPins.decode(pins)),
                reconnecting: store.isLoaded && store.link != .live,
                loadError: store.loadError,
                focus: $router.agentsFocus,
                onSnooze: { key, option in act { await store.snooze(key, option) } },
                onUnsnooze: { key in act { await store.unsnooze(key) } },
                onPin: { key, on in withAnimation(Lapis.Motion.standard) { pins = FleetPins.set(key, pinned: on, in: pins) } },
                onRetry: { Task { await store.refresh() } },
                onRowShown: { key in store.warm(key) },
                talkWaiting: store.talk.waiting,
                onHold: { on in if on { confirmHold = true } else { act { await store.command.hold(false) } } },
                onReview: { item in router.approval = AttentionRef(id: item.id) }
            )
        }
        .confirmationDialog("Hold all?", isPresented: $confirmHold, titleVisibility: .visible) {
            Button("Hold all", role: .destructive) { act { await store.command.hold(true) } }
        } message: {
            Text("Every Mac stops taking commands and typing queued prompts until you release it. Running turns finish.")
        }
        .onChange(of: store.command.notice) { _, notice in
            guard let notice else { return }
            toast = ToastMessage(text: notice.text, tone: notice.isError ? .error : .neutral)
            store.command.notice = nil
        }
        .refreshable { await store.refresh() }
        .lapisToast($toast)
        .onAppear { appeared = true }
        .onDisappear {
            appeared = false
            // gone, not just covered: a removed view sees no change to react
            // to, so it stops counting here (or every later notification
            // would be taken for one about a screen in front)
            presence.update(inFront: false, in: store)
            // a notification's session that never showed is not kept for the next visit
            router.agentsFocus = nil
        }
        .onChange(of: inFront, initial: true) { _, front in
            // back in front: the server's order again
            if presence.update(inFront: front, in: store) { Task { await store.refresh() } }
        }
    }

    private func act(_ change: @escaping @MainActor () async -> String?) {
        Task {
            // "" is a Face ID the person cancelled: nothing to say
            if let failure = await change(), !failure.isEmpty { toast = ToastMessage(text: failure, tone: .error) }
        }
    }
}

struct AgentsContent: View {
    var availability: FleetStore.Availability
    var board: FleetBoard
    /// the fleet showing may be behind: its stream is down
    var reconnecting = false
    var loadError: String?
    /// a session to bring into view and mark (a tapped notification)
    var focus: Binding<FleetSessionKey?> = .constant(nil)
    var onSnooze: (FleetSessionKey, FleetSnooze) -> Void = { _, _ in }
    var onUnsnooze: (FleetSessionKey) -> Void = { _ in }
    var onPin: (FleetSessionKey, Bool) -> Void = { _, _ in }
    var onRetry: () -> Void = {}
    /// a session's row is on screen: its cached turns are read ahead, so a tap opens it drawn
    var onRowShown: (FleetSessionKey) -> Void = { _ in }
    /// what Talk asks you to settle
    var talkWaiting = 0
    var onHold: (Bool) -> Void = { _ in }
    var onReview: (FabricAttentionItem) -> Void = { _ in }

    @State private var showResting = false
    @State private var fabric: String?
    @State private var snoozing: FleetRow?
    @State private var marked: FleetSessionKey?

    var body: some View {
        Group {
            switch availability {
            case .unavailable:
                EmptyState(symbol: "sparkle", title: "Agents live on your Taisce server",
                           message: "This server has no fleet. Sign in to your own Taisce server to watch your Claude sessions here.")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                    .accessibilityIdentifier("agents.unavailable")
            case .unknown where loadError != nil:
                EmptyState(symbol: "wifi.exclamationmark", title: "Your agents didn\u{2019}t load", message: loadError,
                           actionTitle: "Try Again", action: onRetry)
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .unknown:
                list(loading: true)
            case .available:
                list(loading: false)
            }
        }
        .groundBackground()
        .navigationTitle("Agents")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .principal) { Color.clear.frame(width: 1, height: 1).accessibilityHidden(true) }
            if availability == .available, !board.hasNoRuntime {
                ToolbarItem(placement: .topBarTrailing) {
                    Button(board.held ? "Release" : "Hold all", systemImage: board.held ? "play.circle" : "hand.raised") { onHold(!board.held) }
                        .tint(board.held ? Color.lapis.accent : Color.lapis.red)
                        .accessibilityIdentifier("agents.hold")
                }
                ToolbarItem(placement: .topBarTrailing) {
                    NavigationLink(value: Route.agentSettings) { Image(systemName: "gearshape") }
                        .accessibilityLabel("Agent settings")
                        .accessibilityIdentifier("agents.settings")
                }
            }
        }
    }

    private func list(loading: Bool) -> some View {
        ScrollViewReader { proxy in
            List {
                Section {
                    header
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: 4))
                        .listRowSeparator(.hidden)
                }
                if loading {
                    Section {
                        SkeletonBlock(height: 156, radius: Lapis.Radius.tab)
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets())
                    }
                } else if board.hasNoRuntime {
                    Section {
                        EmptyState(symbol: "desktopcomputer", title: "No Mac enrolled yet",
                                   message: "A runtime is the program on your Mac that reports its Claude sessions. Enrol one on your server:\ntaisce fabric runtime add --name <name>\nIt prints the credential that runtime signs in with.")
                            .listRowBackground(Color.clear)
                            .listRowInsets(EdgeInsets())
                            .accessibilityIdentifier("agents.empty.noRuntime")
                    }
                } else {
                    talkAndFilter
                    status
                    if board.hasNoSessions {
                        Section {
                            EmptyCard(icon: "sparkle", title: "No sessions", hint: "Claude sessions started on your Mac show here, the one that needs you first.")
                                .listRowBackground(Color.clear)
                                .listRowInsets(EdgeInsets())
                                .accessibilityIdentifier("agents.empty.noSessions")
                        }
                    }
                    sessions
                }
            }
            .listStyle(.insetGrouped)
            // Hold all is pinned above the list, never a row of it: a list
            // scrolled down to a session would keep it out of sight (and out
            // of the view tree), while the toolbar alone said "Release"
            .safeAreaInset(edge: .top, spacing: 0) {
                if board.held, !loading, !board.hasNoRuntime { heldBanner }
            }
            .listSectionSpacing(18)
            .scrollContentBackground(.hidden)
            .contentMargins(.top, 0, for: .scrollContent)
            .environment(\.defaultMinListRowHeight, 52)
            // reading width on iPad, centred on the ground
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
            .confirmationDialog("Snooze notifications", isPresented: Binding(get: { snoozing != nil }, set: { if !$0 { snoozing = nil } }), titleVisibility: .visible, presenting: snoozing) { row in
                ForEach(FleetSnooze.allCases, id: \.self) { option in
                    Button(option.title()) { onSnooze(row.id, option) }
                }
            } message: { row in
                Text("\(row.session.address) stays in the list.")
            }
            .task(id: FocusTrigger(key: focus.wrappedValue, listed: lists(focus.wrappedValue), loaded: !loading)) {
                await show(focus.wrappedValue, proxy)
            }
        }
    }

    /// What restarts the focus task: another session asked for, or the one
    /// asked for arriving in the fleet. (Not the binding being cleared by
    /// the task itself: that happens last.)
    private struct FocusTrigger: Equatable {
        var key: FleetSessionKey?
        var listed: Bool
        var loaded: Bool
    }

    private func lists(_ key: FleetSessionKey?) -> Bool {
        guard let key else { return false }
        return (board.pinned + board.rows + board.resting).contains { $0.id == key }
    }

    /// Bring a notification's session into view and mark it for a moment.
    /// One the fleet has not delivered yet stays asked for until it does
    /// (or another is asked for, or the screen goes).
    private func show(_ key: FleetSessionKey?, _ proxy: ScrollViewProxy) async {
        guard let key, lists(key) else { return }
        if board.resting.contains(where: { $0.id == key }) { showResting = true }
        do {
            // after the list (and the fold, if it opened) has laid out
            try await Task.sleep(for: .milliseconds(250))
            withAnimation { proxy.scrollTo(key, anchor: .center) }
            marked = key
            try await Task.sleep(for: Self.markTime)
        } catch {
            // another session was asked for: its own task takes over
            if marked == key { marked = nil }
            return
        }
        withAnimation(Lapis.Motion.standard) { if marked == key { marked = nil } }
        if focus.wrappedValue == key { focus.wrappedValue = nil }
    }

    /// How long a notification's row stays marked.
    static let markTime: Duration = .seconds(3)

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Agents")
                .lapisFont(.display)
                .foregroundStyle(Color.lapis.ink)
                .accessibilityAddTraits(.isHeader)
            let line = [reconnecting ? "Reconnecting\u{2026}" : nil, board.summary].compactMap { $0 }
            if !line.isEmpty {
                Text(line.joined(separator: " \u{00B7} ")).font(.subheadline).foregroundStyle(Color.lapis.ink3)
                    .accessibilityIdentifier("agents.summary")
            }
        }
        .padding(.leading, 6)
        .padding(.trailing, 2)
        .padding(.top, 10)
    }

    private var heldBanner: some View {
        LapisBanner(text: "Hold all is on: your Macs take nothing but its release.", tone: .yellow, symbol: "hand.raised.fill",
                    actionTitle: "Release", action: { onHold(false) })
            .padding(.horizontal, Lapis.Layout.gutter)
            .padding(.vertical, 8)
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
            // one element holding the words and the Release button
            .accessibilityElement(children: .contain)
            .accessibilityIdentifier("agents.held")
    }

    /// Talk, with what it asks you to settle; the fabric filter.
    @ViewBuilder private var talkAndFilter: some View {
        Section {
            NavigationLink(value: Route.talk) {
                HStack(spacing: 12) {
                    Image(systemName: "bubble.left.and.bubble.right")
                        .foregroundStyle(Color.lapis.agent)
                        .frame(width: 24)
                    Text("Talk").font(.subheadline.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                    Text("agents asking each other").font(.caption).foregroundStyle(Color.lapis.ink3)
                    Spacer(minLength: 0)
                    CountPill(n: talkWaiting, tone: .attention, label: "\(talkWaiting) waiting for you")
                }
                .frame(minHeight: 44)
            }
            .listRowBackground(Color.lapis.card)
            .accessibilityIdentifier("agents.talk")
        }
        if board.fabrics.count > 1 {
            Section {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        FilterChip(title: "All", selected: fabric == nil) { fabric = nil }
                        ForEach(board.fabrics, id: \.self) { f in
                            FilterChip(title: f, selected: fabric == f) { fabric = fabric == f ? nil : f }
                                .accessibilityIdentifier("agents.fabric.\(f)")
                        }
                    }
                    .padding(.horizontal, 4)
                }
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.clear)
            }
        }
    }

    /// Which Mac is awake, and each account's headroom.
    @ViewBuilder private var status: some View {
        Section {
            ForEach(board.runtimes) { note in
                FleetRuntimeLine(note: note)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.lapis.card)
                    .listRowSeparatorTint(Color.lapis.line2)
            }
            ForEach(board.headroom) { account in
                FleetHeadroomLine(account: account)
                    .listRowInsets(EdgeInsets())
                    .listRowBackground(Color.lapis.card)
                    .listRowSeparatorTint(Color.lapis.line2)
            }
        }
    }

    @ViewBuilder private var sessions: some View {
        let board = board.only(fabric)
        if !board.pinned.isEmpty {
            Section {
                ForEach(board.pinned) { row in sessionRow(row, pinned: true) }
            } header: {
                Label("Pinned", systemImage: "pin.fill")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(Color.lapis.ink3)
                    .textCase(nil)
                    .accessibilityIdentifier("agents.pinned")
            }
        }
        if !board.rows.isEmpty {
            Section {
                ForEach(board.rows) { row in sessionRow(row) }
            }
        }
        if !board.resting.isEmpty {
            Section {
                Button { withAnimation(Lapis.Motion.standard) { showResting.toggle() } } label: {
                    HStack(spacing: 6) {
                        Text("Idle, asleep, exited").foregroundStyle(Color.lapis.ink2)
                        Text("\(board.resting.count)").foregroundStyle(Color.lapis.ink3).monospacedDigit()
                        Spacer(minLength: 0)
                        Image(systemName: "chevron.right")
                            .font(.footnote.weight(.semibold))
                            .foregroundStyle(Color.lapis.ink3)
                            .rotationEffect(.degrees(showResting ? 90 : 0))
                            .accessibilityHidden(true)
                    }
                    .font(.subheadline.weight(.medium))
                    .padding(.horizontal, 16)
                    .frame(minHeight: Lapis.Layout.minTarget)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .listRowInsets(EdgeInsets())
                .listRowBackground(Color.lapis.card)
                .listRowSeparatorTint(Color.lapis.line2)
                .accessibilityLabel("Idle, asleep, exited, \(board.resting.count)")
                .accessibilityValue(showResting ? "shown" : "hidden")
                .accessibilityIdentifier("agents.resting")
                if showResting {
                    ForEach(board.resting) { row in sessionRow(row) }
                }
            }
        }
    }

    private func sessionRow(_ row: FleetRow, pinned: Bool = false) -> some View {
        NavigationLink(value: Route.agentSession(row.id)) {
            FleetRowView(row: row)
        }
            .onAppear { onRowShown(row.id) }
            .listRowInsets(EdgeInsets(top: 0, leading: 0, bottom: 0, trailing: 12))
            .listRowBackground(marked == row.id ? Color.lapis.accentSoft : Color.lapis.card)
            .listRowSeparatorTint(Color.lapis.line2)
            .id(row.id)
            .swipeActions(edge: .leading, allowsFullSwipe: true) {
                Button(pinned ? "Unpin" : "Pin", systemImage: pinned ? "pin.slash" : "pin") { onPin(row.id, !pinned) }
                    .tint(Color.lapis.accent)
            }
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                if row.snoozed == nil {
                    Button("Snooze", systemImage: "bell.slash") { snoozing = row }.tint(Color.lapis.yellowFill)
                } else {
                    Button("Unsnooze", systemImage: "bell") { onUnsnooze(row.id) }.tint(Color.lapis.accent)
                }
            }
            .contextMenu {
                if let item = row.waitingItem, item.kind == .approval {
                    Button("Review approval", systemImage: "checkmark.shield") { onReview(item) }
                }
                Button(pinned ? "Unpin" : "Pin to top", systemImage: pinned ? "pin.slash" : "pin") { onPin(row.id, !pinned) }
                if row.snoozed == nil {
                    Menu("Snooze notifications", systemImage: "bell.slash") {
                        ForEach(FleetSnooze.allCases, id: \.self) { option in Button(option.title()) { onSnooze(row.id, option) } }
                    }
                } else {
                    Button("Unsnooze", systemImage: "bell") { onUnsnooze(row.id) }
                }
                Button("Copy address", systemImage: "doc.on.doc") { UIPasteboard.general.string = row.session.address }
            }
            // the row a notification pointed at
            .accessibilityAddTraits(marked == row.id ? .isSelected : [])
            .accessibilityAction(named: pinned ? "Unpin" : "Pin to top") { onPin(row.id, !pinned) }
            .accessibilityAction(named: row.snoozed == nil ? "Snooze notifications" : "Unsnooze") {
                if row.snoozed == nil { snoozing = row } else { onUnsnooze(row.id) }
            }
    }
}

/// One session: its state's mark, its address, what state it is in and for
/// how long, the question or last line, and what it runs on. Dimmed when
/// its runtime is away (this is only what was last known).
struct FleetRowView: View {
    let row: FleetRow
    @Environment(\.dynamicTypeSize) private var typeSize

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: row.session.state.symbol)
                .font(.body.weight(.medium))
                .foregroundStyle(row.tone.mark)
                .frame(width: 24)
                .padding(.top, 1)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 4) {
                Text(row.session.address)
                    .font(.system(.body, design: .monospaced).weight(.medium))
                    .foregroundStyle(Color.lapis.ink)
                    .lineLimit(typeSize.isAccessibilitySize ? 3 : 1)
                    .truncationMode(.middle)
                FlowRow(spacing: 6) {
                    StatusChip(text: row.status, tone: row.tone)
                        .accessibilityIdentifier("agents.state.\(row.session.address)")
                    if let waiting = row.waiting {
                        StatusChip(text: waiting, tone: row.waitingItem?.kind == .tamper ? .red : .yellow, symbol: "hand.raised")
                    }
                    if let snoozed = row.snoozed {
                        StatusChip(text: snoozed, tone: .neutral, symbol: "bell.slash")
                            .accessibilityIdentifier("agents.snoozed.\(row.session.address)")
                    }
                    if row.queued > 0 {
                        StatusChip(text: "\(row.queued) queued", tone: .accent, symbol: "text.append")
                    }
                }
                if let detail = row.detail {
                    Text(detail)
                        .font(row.waitingItem?.kind == .approval ? .system(.subheadline, design: .monospaced) : .subheadline)
                        .foregroundStyle(Color.lapis.ink2)
                        .lineLimit(typeSize.isAccessibilitySize ? 6 : 3)
                }
                if let meta = row.meta {
                    Text(meta)
                        .font(.caption)
                        .foregroundStyle(row.contextNearlyFull ? Color.lapis.yellow : Color.lapis.ink3)
                        .lineLimit(typeSize.isAccessibilitySize ? 3 : 1)
                        .truncationMode(.middle)
                }
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 12)
        .frame(minHeight: 52)
        .opacity(row.offline ? 0.55 : 1)
        .contentShape(.rect)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(row.spoken)
        .accessibilityValue(row.session.state.words)
        .accessibilityIdentifier("agents.row.\(row.session.address)")
    }
}

/// "Mac awake" / "Mac awake · no sessions" / "Mac asleep since 12:41", and which runtime.
struct FleetRuntimeLine: View {
    let note: FleetRuntimeNote

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: note.awake ? "desktopcomputer" : "moon.zzz")
                .font(.body)
                .foregroundStyle(note.online ? Color.lapis.green : Color.lapis.ink3)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(note.title).font(.subheadline.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                Text(note.detail).font(.caption).foregroundStyle(Color.lapis.ink3)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(minHeight: 52)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("agents.runtime.\(note.name)")
    }
}

/// One Claude account's headroom, with a warning when it is nearly spent
/// and sessions are running on it.
struct FleetHeadroomLine: View {
    let account: FleetHeadroom

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Image(systemName: account.warning ? "exclamationmark.triangle" : "gauge.with.dots.needle.33percent")
                .font(.body)
                .foregroundStyle(account.warning ? Color.lapis.yellowMark : Color.lapis.ink3)
                .frame(width: 24)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(account.name).font(.subheadline.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                let lines = [account.session, account.week].compactMap { $0 }
                Text(lines.isEmpty ? "headroom not reported" : lines.joined(separator: "\n"))
                    .font(.caption)
                    .foregroundStyle(account.warning ? Color.lapis.yellow : Color.lapis.ink3)
            }
            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 10)
        .frame(minHeight: 52)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(account.spoken)
        .accessibilityIdentifier("agents.account.\(account.name)")
    }
}

#if DEBUG
#Preview("Agents") {
    NavigationStack {
        AgentsContent(availability: .available, board: FleetBoard(FleetPreview.fleet, now: FleetPreview.now))
    }
    .preferredColorScheme(.light)
}

#Preview("Agents, Mac asleep, dark") {
    NavigationStack {
        AgentsContent(availability: .available, board: FleetBoard(FleetPreview.asleep, now: FleetPreview.now), reconnecting: true)
    }
    .preferredColorScheme(.dark)
}

#Preview("Agents, no Mac") {
    NavigationStack { AgentsContent(availability: .available, board: FleetBoard(Fleet(), now: FleetPreview.now)) }
}

#Preview("Agents, not here") {
    NavigationStack { AgentsContent(availability: .unavailable, board: FleetBoard()) }
}

enum FleetPreview {
    static let now = Date(timeIntervalSince1970: 1_800_000_600)
    static let since = 1_800_000_000

    static func session(_ n: Int, _ address: String, _ state: SessionState, tool: String = "", last: String = "", quiet: Int? = nil) -> FleetSession {
        FleetSession(sessionID: "s\(n)", runtimeID: "r1", address: address, branch: "main", account: n % 2 == 0 ? "work" : "personal",
                     state: state, tool: tool, lastLine: last, contextUsedPct: 30 + n * 14, since: since, updatedAt: since, quietUntil: quiet)
    }

    static let sessions = [
        session(1, "taisce/main", .needsYou, last: "The tests pass. Shall I open the pull request?"),
        session(2, "qompass/mcp-dx", .stalled, last: "Waiting for a password", quiet: since + 4200),
        session(3, "workbox/main", .finished, last: "Done: the hook receiver checks Host, Origin and the token."),
        session(4, "taisce/fabric", .working, tool: "Bash"),
        session(5, "notes/main", .idle),
    ]

    static let accounts = [
        FleetAccount(name: "personal", sessionUsedPct: 38, sessionResetsAt: since + 7200, weekUsedPct: 61),
        FleetAccount(name: "work", sessionUsedPct: 92, weekUsedPct: 70),
    ]

    static let fleet = Fleet(rev: 9, runtimes: [FleetRuntime(id: "r1", name: "mbp", online: true, lastSeenAt: since + 600, accounts: accounts)], sessions: sessions)
    static let asleep = Fleet(rev: 9, runtimes: [FleetRuntime(id: "r1", name: "mbp", online: false, lastSeenAt: since, accounts: accounts)], sessions: sessions)
}
#endif
