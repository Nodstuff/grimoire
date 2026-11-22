import SwiftUI
import TaisceKit

/// One session (Slices 2–4): what it said and did, turn by turn; what waits
/// for you (an approval, a question); what is queued; and a reply box that
/// replies, queues or answers. Everything it sends is a signed order, held
/// five seconds first so it can be taken back.
struct AgentSessionScreen: View {
    let key: FleetSessionKey
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.scenePhase) private var scenePhase
    @State private var draft = ""
    @State private var toast: ToastMessage?
    @State private var confirmClear = false
    /// what this screen has on its way (a second tap does nothing)
    @State private var busy = InFlight()
    /// where the feed is scrolled; it opens at the bottom
    @State private var position = ScrollPosition(idType: String.self, edge: .bottom)
    /// the person is at (or very near) the newest turn: a new one scrolls into view
    @State private var nearBottom = true
    /// where the first drawn turn sits in the visible region (y from its top,
    /// and its height), and that region's height: Earlier turns puts it back
    /// exactly there once older turns are in above it
    @State private var firstRow = CGRect.zero
    @FocusState private var composing: Bool

    init(key: FleetSessionKey) {
        self.key = key
    }

    private var store: FleetStore { model.fleet }
    /// kept by the store across opens: a session opened before draws at once
    private var feed: SessionFeed { store.feed(key) }
    private var command: CommandCenter { store.command }
    private var session: FleetSession? { store.fleet.session(key) }
    private var runtime: FleetRuntime? { store.fleet.runtime(key.runtimeID) }

    var body: some View {
        Group {
            if let session {
                // only what says a relative time redraws on the 30 s tick
                // (the header, the running-now line), never the turn list
                content(session)
            } else {
                EmptyState(symbol: "sparkle", title: "This session is gone", message: "Its Mac no longer reports it.")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .groundBackground()
        .navigationTitle(session?.address ?? "Session")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar { toolbar }
        .lapisToast($toast)
        .task {
            // what this device last saw at once, then the stream's turns from
            // this run (the store merges later ones itself), then only what
            // ended after the cache's watermark
            await feed.load(store.fabricService, cache: store.turnCache, live: store.liveTurns[key] ?? [])
            await command.loadOrders(for: key)
        }
        .onChange(of: scenePhase) { _, phase in
            // back from the background: what ended meanwhile (the stream was down)
            if phase == .active, feed.opened { Task { await feed.refresh(store.fabricService, cache: store.turnCache) } }
        }
        .onChange(of: command.notice) { _, notice in
            guard let notice else { return }
            toast = ToastMessage(text: notice.text, tone: notice.isError ? .error : .neutral)
            command.notice = nil
        }
        .onChange(of: feed.problem) { _, problem in
            guard let problem else { return }
            toast = ToastMessage(text: problem.text, tone: .error)
            feed.clearProblem()
        }
        .onAppear { store.sessionOnScreen(key) }
        .onDisappear {
            store.sessionOnScreen(nil)
            feed.looked()
        }
        .task(id: SeenWhile(phase: scenePhase, state: session?.state)) {
            // the person has it on screen, in the front, and it has finished
            // (they opened it, came back to it, or watched it finish): tell
            // the Mac it was seen (FINISHED becomes IDLE). Only while they
            // look: a state the server reports never signs SEEN by itself.
            guard scenePhase == .active, session?.state == .finished, let runtime, runtime.online, command.isPaired(with: runtime) else { return }
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, scenePhase == .active, session?.state == .finished else { return }
            await command.command(.seen, key)
        }
        .confirmationDialog("Clear the conversation?", isPresented: $confirmClear, titleVisibility: .visible) {
            Button("Clear", role: .destructive) { act("clear") { await command.command(.clear, key) } }
        } message: {
            Text("Claude Code's /clear: the session starts again with an empty context.")
        }
    }

    // MARK: content

    private func content(_ s: FleetSession) -> some View {
        let items = store.fleet.attention(for: key)
        // what each item is comes from what the Mac signed, never the server's kind
        // (memoised in the command center: no Keychain read or verify per redraw)
        let question = items.first { command.verifiedKind($0) == .question }
        let mode = ComposerMode(s, runtime: runtime)
        return Group {
            if feed.opened {
                feedScroll(s, items: items, question: question)
            } else {
                // the cache is being read (milliseconds): nothing, not an empty state
                Color.clear.frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .safeAreaInset(edge: .bottom) {
            composer(s, mode: mode, question: question)
        }
    }

    private func feedScroll(_ s: FleetSession, items: [FabricAttentionItem], question: FabricAttentionItem?) -> some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: 14) {
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    headerCard(SessionHeader(s, runtime: runtime, now: context.date), s)
                }
                if feed.hasEarlier {
                    Button { showEarlier() } label: {
                        Label(feed.loadingOlder ? "Loading\u{2026}" : "Earlier turns", systemImage: "arrow.up.circle")
                            .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.lapis(.ghost))
                    .disabled(feed.loadingOlder)
                    .accessibilityIdentifier("session.earlier")
                }
                if feed.rows.isEmpty {
                    if feed.loading {
                        ProgressView().frame(maxWidth: .infinity).padding(.vertical, 24).accessibilityIdentifier("session.loading")
                    } else if let error = feed.loadError {
                        VStack(alignment: .leading, spacing: 10) {
                            EmptyCard(icon: "wifi.exclamationmark", title: error, hint: "The turns couldn't be read. Pull down, or try again.")
                            Button("Try again", systemImage: "arrow.clockwise") {
                                Task { await feed.refresh(store.fabricService, cache: store.turnCache) }
                            }
                            .buttonStyle(.lapis(.secondary, size: .small))
                            .accessibilityIdentifier("session.retry")
                        }
                    } else {
                        EmptyCard(icon: "text.bubble", title: "No turns yet",
                                  hint: s.textOff ? "This project has Text off: turns are not kept." : "What it is asked and what it answers shows here, turn by turn.")
                    }
                }
                ForEach(feed.rows) { row in
                    VStack(alignment: .leading, spacing: 14) {
                        if row.id == feed.firstNew, feed.newCount > 0 {
                            newDivider(feed.newCount)
                        }
                        TurnView(row: row, whole: feed.expanded[row.id]) { feed.expand(row.id, $0) }.equatable()
                    }
                    .id(row.id)
                    .onGeometryChange(for: CGRect.self) { $0.frame(in: .scrollView) } action: { frame in
                        if row.id == feed.rows.first?.id { firstRow = frame }
                    }
                }
                if s.state == .working || s.state == .stalled {
                    TimelineView(.periodic(from: .now, by: 30)) { context in runningNow(s, now: context.date) }
                }
                // everything that asks something of the person sits at the bottom,
                // where the feed opens and the reply box is, never above the turns
                ForEach(items.filter { $0.id != question?.id }) { item in
                    AttentionCard(item: item, verifiedKind: command.verifiedKind(item), onOpen: { router.approval = AttentionRef(id: item.id) })
                }
                if let question { QuestionCard(item: question, command: command, onDone: done) }
                if runtime?.held == true { heldBanner }
                // a paused queue says why, and is released, on the waiting message itself
                ForEach(s.queue, id: \.self) { id in queuedCard(id, s) }
                if let runtime, !command.isPaired(with: runtime) { pairNote(runtime) }
                ForEach(command.pending.filter { $0.session == key }) { p in
                    UndoCard(pending: p, retrying: command.inFlight.contains(p.id),
                             onUndo: { draft = command.undo(p.id) ?? draft },
                             onRetry: { Task { await command.retry(p.id) } })
                }
                Color.clear.frame(height: 1).id("bottom")
            }
            .padding(.horizontal, Lapis.Layout.gutter)
            .padding(.vertical, 12)
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
            .scrollTargetLayout()
        }
        .scrollPosition($position)
        .scrollDismissesKeyboard(.interactively)
        // it opens at the newest turn, already drawn there: no jump to the bottom
        .defaultScrollAnchor(.bottom, for: .initialOffset)
        .onScrollGeometryChange(for: Bool.self) { g in
            g.contentSize.height - g.visibleRect.maxY < 160
        } action: { _, near in
            nearBottom = near
        }
        .onScrollGeometryChange(for: CGFloat.self) { $0.contentInsets.top } action: { _, top in insetTop = top }
        .onScrollGeometryChange(for: CGFloat.self) { $0.contentOffset.y } action: { _, y in offsetY = y }
        // a new turn comes into view only when the person was at the bottom:
        // not while they read older ones, and never when older ones load above
        .onChange(of: feed.rows.last?.id) { old, new in
            guard new != nil, nearBottom || old == nil else { return }
            withAnimation(old == nil ? nil : .snappy) { position.scrollTo(edge: .bottom) }
        }
        // the person sent something: show it waiting
        .onChange(of: command.pending.count) { old, new in
            if new > old { withAnimation(.snappy) { position.scrollTo(edge: .bottom) } }
        }
        .onChange(of: composing) { _, on in
            if on, nearBottom { withAnimation(.snappy) { position.scrollTo(edge: .bottom) } }
        }
        .refreshable { await feed.refresh(store.fabricService, cache: store.turnCache) }
    }

    /// Older turns go in above the first drawn one, and that one stays where
    /// the person sees it: a scroll view keeps its offset, so without this the
    /// new turns would push it down and out of sight. The turn is scrolled to
    /// the top of the visible region (exact, whatever the new turns' heights),
    /// then down by how far below that top it was.
    private func showEarlier() {
        let anchor = feed.rows.first?.id
        let below = firstRow.minY - insetTop
        Task {
            await feed.showEarlier(store.fabricService, cache: store.turnCache)
            guard let anchor, feed.rows.first?.id != anchor else { return }
            var quiet = Transaction(animation: nil)
            quiet.disablesAnimations = true
            withTransaction(quiet) { position.scrollTo(id: anchor, anchor: .top) }
            // the layout that puts it at the top, then the offset it had under it
            await Task.yield()
            try? await Task.sleep(for: .milliseconds(16))
            withTransaction(quiet) { position.scrollTo(y: offsetY - below) }
        }
    }

    /// how far the feed is scrolled (content offset)
    @State private var offsetY: CGFloat = 0

    /// the visible region's top in the scroll view's space (under the bar)
    @State private var insetTop: CGFloat = 0

    private func headerCard(_ h: SessionHeader, _ s: FleetSession) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(h.address)
                .font(.system(.title3, design: .monospaced).weight(.semibold))
                .foregroundStyle(Color.lapis.ink)
                .textSelection(.enabled)
            if !h.meta.isEmpty {
                Text(h.meta).font(.subheadline).foregroundStyle(h.contextNearlyFull ? Color.lapis.yellow : Color.lapis.ink3)
            }
            FlowRow(spacing: 6) {
                StatusChip(text: h.status, tone: h.tone).accessibilityIdentifier("session.status")
                if !h.place.isEmpty { StatusChip(text: h.place, tone: .neutral, symbol: "desktopcomputer") }
                if s.following { StatusChip(text: "Following", tone: .accent, symbol: "bell.fill") }
                if !s.fabric.isEmpty, s.fabric != s.project { StatusChip(text: s.fabric, tone: .agent, symbol: "square.stack.3d.up") }
                if s.textOff { StatusChip(text: "Text off", tone: .neutral, symbol: "eye.slash") }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("session.header")
    }

    private func newDivider(_ n: Int) -> some View {
        HStack(spacing: 8) {
            Rectangle().fill(Color.lapis.accent).frame(height: 1)
            Text(n == 1 ? "1 new since you looked" : "\(n) new since you looked")
                .font(.caption.weight(.semibold)).foregroundStyle(Color.lapis.accent).fixedSize()
            Rectangle().fill(Color.lapis.accent).frame(height: 1)
        }
        .accessibilityIdentifier("session.newSince")
    }

    private func runningNow(_ s: FleetSession, now: Date) -> some View {
        HStack(alignment: .top, spacing: 10) {
            ProgressView().controlSize(.small)
            VStack(alignment: .leading, spacing: 2) {
                Text("Running now \u{00B7} \(FleetTime.elapsed(since: s.since, now: now))")
                    .font(.caption.weight(.semibold)).foregroundStyle(Color.lapis.accent)
                let line = [s.tool, s.note].filter { !$0.isEmpty }.joined(separator: " \u{00B7} ")
                if !line.isEmpty {
                    Text(line).font(.system(.footnote, design: .monospaced)).foregroundStyle(Color.lapis.ink2).lineLimit(3)
                }
            }
            Spacer(minLength: 0)
        }
        .padding(12)
        .background(Color.lapis.accentSoft.opacity(0.5), in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("session.running")
    }

    private func queuedCard(_ id: String, _ s: FleetSession) -> some View {
        let order = command.orders[id]
        return VStack(alignment: .leading, spacing: 8) {
            Text(s.queuePaused.isEmpty ? "QUEUED \u{00B7} SENT WHEN IT STOPS" : "QUEUED \u{00B7} \(FabricWords.queuePaused(s.queuePaused).uppercased())")
                .font(.caption2.weight(.semibold)).tracking(0.8).foregroundStyle(Color.lapis.ink3)
            Text(order?.text ?? "A queued message")
                .font(.body).foregroundStyle(Color.lapis.ink)
            HStack {
                // a paused queue is released here, where the person is looking,
                // not only from the banner at the top of the feed
                if !s.queuePaused.isEmpty {
                    Button("Send now", systemImage: "paperplane") { act("release") { await command.command(.releaseQueue, key) } }
                        .buttonStyle(.lapis(.primary, size: .small))
                        .disabled(busy.isRunning("release"))
                        .accessibilityIdentifier("session.queued.release")
                }
                if let text = order?.text {
                    Button("Edit", systemImage: "pencil") {
                        act("queued.\(id)") {
                            let failure = await command.command(.cancel, key, orderIDs: [id])
                            if failure == nil { draft = text; composing = true }
                            return failure
                        }
                    }
                    .buttonStyle(.lapis(.secondary, size: .small))
                    .disabled(busy.isRunning("queued.\(id)"))
                }
                Button("Cancel", systemImage: "xmark") { act("queued.\(id)") { await command.command(.cancel, key, orderIDs: [id]) } }
                    .buttonStyle(.lapis(.destructive, size: .small))
                    .disabled(busy.isRunning("queued.\(id)"))
                    .accessibilityIdentifier("session.queued.cancel")
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.card, style: .continuous).strokeBorder(Color.lapis.line2, style: StrokeStyle(lineWidth: 1, dash: [5, 4])))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("session.queued")
    }

    private func pairNote(_ r: FleetRuntime) -> some View {
        LapisBanner(text: "To reply from here, pair this phone with \(r.name): Settings \u{203A} Agents \u{203A} Pair with a Mac.", tone: .accent, symbol: "lock.shield")
            .accessibilityIdentifier("session.pairNote")
    }

    private var heldBanner: some View {
        LapisBanner(text: "Hold all is on for this Mac: nothing you send runs until you release it.", tone: .yellow, symbol: "hand.raised.fill")
    }

    // MARK: composer

    private func composer(_ s: FleetSession, mode: ComposerMode, question: FabricAttentionItem?) -> some View {
        VStack(spacing: 8) {
            if mode.canType, question == nil, s.state == .needsYou || s.state == .finished, draft.isEmpty {
                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(QuickReplies.stock, id: \.self) { reply in
                            Button(reply) { send(reply, s, mode) }
                                .buttonStyle(.lapis(.secondary, size: .small))
                                .accessibilityIdentifier("session.quick.\(reply)")
                        }
                    }
                    .padding(.horizontal, Lapis.Layout.gutter)
                }
            }
            if mode == .asleep || mode == .gone {
                Button { act("resume") { await command.command(.resume, key) } } label: {
                    Label("Resume \(s.address)", systemImage: "play.fill").frame(maxWidth: .infinity)
                }
                .buttonStyle(.lapis(.primary))
                .disabled(busy.isRunning("resume"))
                .padding(.horizontal, Lapis.Layout.gutter)
                .accessibilityIdentifier("session.resume")
            } else {
                HStack(alignment: .bottom, spacing: 8) {
                    TextField(question != nil ? "Answer in your own words" : mode.placeholder, text: $draft, axis: .vertical)
                        .lineLimit(1...6)
                        .focused($composing)
                        .disabled(!mode.canType && question == nil)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 10)
                        .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
                        .overlay(RoundedRectangle(cornerRadius: 20, style: .continuous).strokeBorder(Color.lapis.line2))
                        .accessibilityIdentifier("session.composer")
                    Button {
                        if let question { answer(question) } else { send(draft, s, mode) }
                    } label: {
                        Group {
                            if busy.isRunning("answer") {
                                ProgressView().tint(Color.lapis.onAccent)
                            } else {
                                Image(systemName: mode.queues && question == nil ? "text.append" : "arrow.up")
                                    .font(.body.weight(.bold))
                                    .foregroundStyle(Color.lapis.onAccent)
                            }
                        }
                        .frame(width: 38, height: 38)
                        .background(Color.lapis.accent, in: .circle)
                        .opacity(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? 0.45 : 1)
                        // the circle is 38 pt; what takes the tap is 44
                        .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    // a hardware keyboard (iPad, the Mac): Command-Return sends
                    .keyboardShortcut(.return, modifiers: .command)
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || busy.isRunning("answer"))
                    .accessibilityLabel(question != nil ? "Answer" : mode.queues ? "Queue" : "Send")
                    .accessibilityIdentifier("session.send")
                }
                .padding(.leading, Lapis.Layout.gutter)
                .padding(.trailing, Lapis.Layout.gutter - 3)
            }
        }
        .padding(.vertical, 10)
        .background(.bar)
    }

    private func send(_ text: String, _ s: FleetSession, _ mode: ComposerMode) {
        // a second tap after the box was cleared: nothing to send, nothing to say
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        if let problem = command.send(text, to: s, queued: mode.queues) {
            toast = ToastMessage(text: problem, tone: .error)
            return
        }
        draft = ""
        Haptics.success()
    }

    private func answer(_ item: FabricAttentionItem) {
        let words = draft
        switch command.verify(item) {
        case .success(let v):
            // disabled while it is on its way: a second tap would answer twice
            act("answer") {
                let failure = await command.answer(v, words: words)
                if failure == nil, draft == words { draft = "" }
                return failure
            }
        case .failure(let p):
            toast = ToastMessage(text: FabricWords.problem(p), tone: .error)
        }
    }

    private func done(_ failure: String?) {
        if let failure, !failure.isEmpty { toast = ToastMessage(text: failure, tone: .error) }
    }

    // MARK: toolbar

    @ToolbarContentBuilder private var toolbar: some ToolbarContent {
        if let s = session {
            if s.state == .working || s.state == .stalled {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Stop", systemImage: "stop.fill") { act("stop") { await command.command(.interrupt, key) } }
                        .tint(Color.lapis.red)
                        .disabled(busy.isRunning("stop"))
                        .accessibilityIdentifier("session.stop")
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu("More", systemImage: "ellipsis.circle") {
                    Button(s.following ? "Stop following" : "Follow every turn", systemImage: s.following ? "bell.slash" : "bell") {
                        act("follow") { await store.follow(key, !s.following) }
                    }
                    Menu("Move to fabric", systemImage: "square.stack.3d.up") {
                        Button("\(s.project.isEmpty ? "Its project" : s.project) (its own)") { act("fabric") { await command.command(.setFabric, key, text: "") } }
                        ForEach(store.config.fabrics) { f in
                            Button(f.name) { act("fabric") { await command.command(.setFabric, key, text: f.id) } }
                        }
                    }
                    Divider()
                    if s.state == .asleep || s.state == .exited {
                        Button("Resume", systemImage: "play") { act("resume") { await command.command(.resume, key) } }
                    }
                    Button("Compact context", systemImage: "arrow.down.right.and.arrow.up.left") { act("compact") { await command.command(.compact, key) } }
                    Button("Clear conversation\u{2026}", systemImage: "trash", role: .destructive) { confirmClear = true }
                    Divider()
                    Button("Copy address", systemImage: "doc.on.doc") { UIPasteboard.general.string = s.address }
                }
                .accessibilityIdentifier("session.more")
            }
        }
    }

    /// Run an action by name, once at a time: a tap while the same one is
    /// on its way does nothing (its button is disabled meanwhile).
    private func act(_ name: String, _ change: @escaping @MainActor () async -> String?) {
        Task {
            if case .some(let failure?) = await busy.run(name, change), !failure.isEmpty {
                toast = ToastMessage(text: failure, tone: .error)
            }
        }
    }
}

/// One turn: what started it (your words as a bubble, or a muted line for
/// what the Mac started it with), what it did in one line, its reply.
/// Equatable on its row (and whether it is shown whole), so a redraw of
/// the feed skips a turn that did not change.
struct TurnView: View, Equatable {
    let row: TurnRow
    /// the whole reply's blocks when the person asked for all of it
    let whole: [ReplyNode]?
    let onExpand: (Bool) -> Void

    nonisolated static func == (a: TurnView, b: TurnView) -> Bool { a.row == b.row && (a.whole == nil) == (b.whole == nil) }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if row.textOff {
                Text("Text off: this turn's words are not kept.").font(.footnote).italic().foregroundStyle(Color.lapis.ink3)
            } else {
                ForEach(row.promptDisplay.notes, id: \.self) { note in
                    Label(note.text, systemImage: note.symbol)
                        .font(.footnote)
                        .foregroundStyle(Color.lapis.ink3)
                        .lineLimit(2)
                        .padding(.horizontal, 10)
                        .padding(.vertical, 5)
                        .background(Color.lapis.card2, in: .capsule)
                        .frame(maxWidth: .infinity)
                        .accessibilityIdentifier("turn.note")
                }
                if !row.promptDisplay.words.isEmpty {
                    HStack {
                        Spacer(minLength: 40)
                        VStack(alignment: .trailing, spacing: 3) {
                            Text(row.promptDisplay.words)
                                .font(.body)
                                .foregroundStyle(Color.lapis.ink)
                                .padding(.horizontal, 14)
                                .padding(.vertical, 10)
                                .background(Color.lapis.accentSoft, in: .rect(cornerRadius: 18, style: .continuous))
                                .textSelection(.enabled)
                            if let source = row.source {
                                Text(source).font(.caption2).foregroundStyle(Color.lapis.ink3)
                            }
                        }
                    }
                }
            }
            if let activity = row.activity {
                HStack(spacing: 6) {
                    Image(systemName: "hammer").font(.caption).foregroundStyle(Color.lapis.ink3)
                    Text(activity).font(.caption).foregroundStyle(Color.lapis.ink2)
                    if let pr = row.pullRequest {
                        Link(destination: pr) { Label("Pull request", systemImage: "arrow.up.right.square").font(.caption.weight(.semibold)) }
                    }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 6)
                .background(Color.lapis.card2, in: .capsule)
            }
            if !row.reply.isEmpty {
                ReplyView(nodes: whole ?? row.replyNodes)
                    .accessibilityIdentifier("turn.reply")
                if row.long {
                    Button(whole == nil ? "Show all" : "Show less") { onExpand(whole == nil) }
                        .buttonStyle(.lapis(.ghost, size: .small))
                        .accessibilityIdentifier("turn.showAll")
                }
            }
            HStack(spacing: 6) {
                if let ending = row.ending { StatusChip(text: ending, tone: row.endTone) }
                if row.truncated { StatusChip(text: "Cut to fit", tone: .neutral) }
                Text(row.when).font(.caption2).foregroundStyle(Color.lapis.ink3)
            }
        }
        .padding(.vertical, 4)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("turn.\(row.id)")
    }
}

/// A held send, going in a few seconds unless taken back.
struct UndoCard: View {
    let pending: PendingSend
    /// a retry is on its way
    var retrying = false
    let onUndo: () -> Void
    var onRetry: () -> Void = {}

    var body: some View {
        if pending.failure != nil { failed } else if pending.lapsed { lapsed } else { counting }
    }

    /// Posted, and the post failed: the words are kept, to send again or take back.
    private var failed: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Not sent: \(pending.failure ?? "")")
                    .font(.caption.weight(.semibold)).foregroundStyle(Color.lapis.red).lineLimit(3)
                Text(pending.text).font(.subheadline).foregroundStyle(Color.lapis.ink).lineLimit(2)
            }
            Spacer(minLength: 0)
            Button("Edit", action: onUndo)
                .buttonStyle(.lapis(.secondary, size: .small))
                .disabled(retrying)
                .accessibilityIdentifier("session.failed.edit")
            Button(retrying ? "Sending\u{2026}" : "Retry", action: onRetry)
                .buttonStyle(.lapis(.primary, size: .small))
                .disabled(retrying)
                .accessibilityIdentifier("session.failed.retry")
        }
        .padding(12)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.card, style: .continuous).strokeBorder(Color.lapis.red.opacity(0.4)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("session.failed")
    }

    /// The app was away past its moment: not sent; back into the box to send again.
    private var lapsed: some View {
        HStack(spacing: 10) {
            VStack(alignment: .leading, spacing: 2) {
                Text("Not sent: the app was away when it was due")
                    .font(.caption.weight(.semibold)).foregroundStyle(Color.lapis.red)
                Text(pending.text).font(.subheadline).foregroundStyle(Color.lapis.ink).lineLimit(2)
            }
            Spacer(minLength: 0)
            Button("Edit", action: onUndo)
                .buttonStyle(.lapis(.secondary, size: .small))
                .accessibilityIdentifier("session.lapsed.edit")
        }
        .padding(12)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("session.lapsed")
    }

    private var counting: some View {
        TimelineView(.periodic(from: .now, by: 0.5)) { context in
            let left = max(0, Int(pending.deadline.timeIntervalSince(context.date).rounded(.up)))
            HStack(spacing: 10) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(pending.queued ? "Queuing in \(left) s" : "Sending in \(left) s")
                        .font(.caption.weight(.semibold)).foregroundStyle(Color.lapis.accent)
                    Text(pending.text).font(.subheadline).foregroundStyle(Color.lapis.ink).lineLimit(2)
                }
                Spacer(minLength: 0)
                Button("Undo", action: onUndo)
                    .buttonStyle(.lapis(.secondary, size: .small))
                    .accessibilityIdentifier("session.undo")
            }
            .padding(12)
            .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.card, style: .continuous).strokeBorder(Color.lapis.accentRing))
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("session.pending")
    }
}

/// An item that waits on you, in the feed: an approval opens its sheet;
/// a dialog or tamper says what to do at the Mac.
struct AttentionCard: View {
    let item: FabricAttentionItem
    /// what the Mac signed it as (nil: a hub item, or a runtime's that did
    /// not verify); only a verified approval can be reviewed
    var verifiedKind: AttentionKind?
    var onOpen: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: symbol).foregroundStyle(tone.mark)
                Text(title).font(.caption.weight(.semibold)).tracking(0.6).foregroundStyle(tone.ink)
            }
            if !item.text.isEmpty {
                Text(item.text)
                    .font(kind == .approval ? .system(.callout, design: .monospaced) : .body)
                    .foregroundStyle(Color.lapis.ink)
                    .lineLimit(4)
            }
            if verifiedKind == .approval {
                Button("Review", systemImage: "checkmark.shield", action: onOpen)
                    .buttonStyle(.lapis(.primary, size: .small))
                    .accessibilityIdentifier("attention.review")
            } else if item.source == .runtime, verifiedKind == nil {
                Text("Not verified against your Mac's key: nothing to act on here.")
                    .font(.caption).foregroundStyle(Color.lapis.ink3)
                    .accessibilityIdentifier("attention.unverified")
            } else if kind == .dialog || kind == .tamper {
                Text(kind == .dialog ? "Claude Code is showing a dialog that needs you at the Mac." : "Its hooks were switched off, so it is held. Deal with it at the Mac.")
                    .font(.caption).foregroundStyle(Color.lapis.ink3)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(tone.soft, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("attention.\(kind.rawValue)")
    }

    /// A runtime's item is what its Mac signed it as; the hub's says itself.
    private var kind: FabricAttentionKind {
        guard item.source == .runtime else { return item.kind }
        switch verifiedKind {
        case .approval?: return .approval
        case .question?: return .question
        case .dialog?: return .dialog
        case .tamper?: return .tamper
        default: return .unknown
        }
    }

    private var tone: LapisTone { kind == .tamper ? .red : .yellow }
    private var symbol: String {
        switch kind {
        case .approval: "checkmark.shield"
        case .dialog: "macwindow"
        case .tamper: "exclamationmark.octagon"
        default: "bell"
        }
    }
    private var title: String {
        switch kind {
        case .approval: "WANTS APPROVAL" + (item.guardrailLabel.isEmpty ? "" : " \u{00B7} \(item.guardrailLabel.uppercased())")
        case .dialog: "A DIALOG AT THE MAC"
        case .tamper: "HELD: HOOKS SWITCHED OFF"
        default: "WAITING"
        }
    }
}

/// A question the agent asked (`ask_human`): its choices as buttons, and
/// the reply box answers it in words. Shown only once verified.
struct QuestionCard: View {
    let item: FabricAttentionItem
    let command: CommandCenter
    var onDone: (String?) -> Void = { _ in }
    @State private var sending: String?

    var body: some View {
        let verified = command.verify(item)
        VStack(alignment: .leading, spacing: 10) {
            Label("IT ASKS YOU", systemImage: "questionmark.bubble")
                .font(.caption.weight(.semibold)).tracking(0.6).foregroundStyle(Color.lapis.yellow)
            switch verified {
            case .success(let v):
                Text(v.request.text).font(.body).foregroundStyle(Color.lapis.ink)
                ForEach(v.request.choices) { choice in
                    Button {
                        sending = choice.id
                        Task {
                            onDone(await command.answer(v, choice: choice))
                            sending = nil
                        }
                    } label: {
                        HStack {
                            Text(choice.label)
                            Spacer(minLength: 0)
                            if sending == choice.id { ProgressView().controlSize(.small) }
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.lapis(.secondary, fullWidth: true))
                    .disabled(sending != nil)
                    .accessibilityIdentifier("question.choice.\(choice.id)")
                }
                Text("Or answer in your own words below.").font(.caption).foregroundStyle(Color.lapis.ink3)
            case .failure(let p):
                Text(item.text).font(.body).foregroundStyle(Color.lapis.ink2)
                Text(FabricWords.problem(p)).font(.caption).foregroundStyle(Color.lapis.red)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color.lapis.yellowSoft, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("attention.question")
    }
}

/// When SEEN is reconsidered: the app came to the front or left it, or the
/// session on screen changed state.
private struct SeenWhile: Hashable {
    let phase: ScenePhase
    let state: SessionState?
}

/// A sheet's handle on an attention item.
struct AttentionRef: Identifiable, Hashable {
    let id: String
}
