import SwiftUI
import TaisceKit

/// Agents: the fleet of Claude sessions the person's runtimes report (Agent
/// Fabric, Slice 1: Watch), by how much each needs them. Nothing here
/// commands a session; the one action is snoozing its notifications.
///
/// The list holds still while it is on screen: a change redraws a row where
/// it stands. The server's order comes back on a pull to refresh and each
/// time the screen comes to the front.
struct AgentsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.scenePhase) private var scenePhase
    @State private var appeared = false
    @State private var counted = false
    @State private var toast: ToastMessage?

    private var store: FleetStore { model.fleet }
    private var inFront: Bool { appeared && scenePhase == .active }

    var body: some View {
        @Bindable var router = router
        // "5 min" moves on while the screen is open
        TimelineView(.periodic(from: .now, by: 30)) { context in
            AgentsContent(
                availability: store.availability,
                board: FleetBoard(store.fleet, now: context.date),
                reconnecting: store.isLoaded && store.link != .live,
                loadError: store.loadError,
                focus: $router.agentsFocus,
                onSnooze: { key, option in act { await store.snooze(key, option) } },
                onUnsnooze: { key in act { await store.unsnooze(key) } },
                onRetry: { Task { await store.refresh() } }
            )
        }
        .refreshable { await store.refresh() }
        .lapisToast($toast)
        .onAppear { appeared = true }
        .onDisappear { appeared = false }
        .onChange(of: inFront, initial: true) { _, front in
            guard front != counted else { return }
            counted = front
            if front {
                store.screenAppeared()
                // back in front: the server's order again
                Task { await store.refresh() }
            } else {
                store.screenDisappeared()
            }
        }
    }

    private func act(_ change: @escaping @MainActor () async -> String?) {
        Task {
            if let failure = await change() { toast = ToastMessage(text: failure, tone: .error) }
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
    var onRetry: () -> Void = {}

    @State private var showResting = false
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
        .toolbar { ToolbarItem(placement: .principal) { Color.clear.frame(width: 1, height: 1).accessibilityHidden(true) } }
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
            .listSectionSpacing(18)
            .scrollContentBackground(.hidden)
            .contentMargins(.top, 0, for: .scrollContent)
            .environment(\.defaultMinListRowHeight, 52)
            // reading width on iPad, centred on the ground
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
            .confirmationDialog("Snooze notifications", isPresented: Binding(get: { snoozing != nil }, set: { if !$0 { snoozing = nil } }), titleVisibility: .visible, presenting: snoozing) { row in
                ForEach(FleetSnooze.allCases, id: \.self) { option in
                    Button(option.title) { onSnooze(row.id, option) }
                }
            } message: { row in
                Text("\(row.session.address) stays in the list.")
            }
            .task(id: FocusKey(key: focus.wrappedValue, loaded: !loading)) { await show(focus.wrappedValue, loaded: !loading, proxy) }
        }
    }

    private struct FocusKey: Equatable {
        var key: FleetSessionKey?
        var loaded: Bool
    }

    /// Bring a notification's session into view and mark it for a moment.
    private func show(_ key: FleetSessionKey?, loaded: Bool, _ proxy: ScrollViewProxy) async {
        guard let key, loaded else { return }
        focus.wrappedValue = nil
        guard (board.rows + board.resting).contains(where: { $0.id == key }) else { return }
        if board.resting.contains(where: { $0.id == key }) { showResting = true }
        // after the fold has opened
        try? await Task.sleep(for: .milliseconds(150))
        withAnimation { proxy.scrollTo(key, anchor: .center) }
        marked = key
        try? await Task.sleep(for: .seconds(3))
        withAnimation(Lapis.Motion.standard) { if marked == key { marked = nil } }
    }

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

    private func sessionRow(_ row: FleetRow) -> some View {
        FleetRowView(row: row)
            .listRowInsets(EdgeInsets())
            .listRowBackground(marked == row.id ? Color.lapis.accentSoft : Color.lapis.card)
            .listRowSeparatorTint(Color.lapis.line2)
            .id(row.id)
            .swipeActions(edge: .trailing, allowsFullSwipe: false) {
                if row.snoozed == nil {
                    Button("Snooze", systemImage: "bell.slash") { snoozing = row }.tint(Color.lapis.yellowFill)
                } else {
                    Button("Unsnooze", systemImage: "bell") { onUnsnooze(row.id) }.tint(Color.lapis.accent)
                }
            }
            .contextMenu {
                if row.snoozed == nil {
                    Menu("Snooze notifications", systemImage: "bell.slash") {
                        ForEach(FleetSnooze.allCases, id: \.self) { option in Button(option.title) { onSnooze(row.id, option) } }
                    }
                } else {
                    Button("Unsnooze", systemImage: "bell") { onUnsnooze(row.id) }
                }
                Button("Copy address", systemImage: "doc.on.doc") { UIPasteboard.general.string = row.session.address }
            }
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
                    if let snoozed = row.snoozed {
                        StatusChip(text: snoozed, tone: .neutral, symbol: "bell.slash")
                            .accessibilityIdentifier("agents.snoozed.\(row.session.address)")
                    }
                }
                if let detail = row.detail {
                    Text(detail)
                        .font(.subheadline)
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

/// "Mac awake" / "Mac asleep since 12:41", and which runtime.
struct FleetRuntimeLine: View {
    let note: FleetRuntimeNote

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: note.online ? "desktopcomputer" : "moon.zzz")
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
