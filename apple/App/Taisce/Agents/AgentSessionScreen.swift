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
    @State private var feed: SessionFeed
    @State private var draft = ""
    @State private var toast: ToastMessage?
    @State private var confirmClear = false
    @FocusState private var composing: Bool

    init(key: FleetSessionKey) {
        self.key = key
        _feed = State(initialValue: SessionFeed(key: key))
    }

    private var store: FleetStore { model.fleet }
    private var command: CommandCenter { store.command }
    private var session: FleetSession? { store.fleet.session(key) }
    private var runtime: FleetRuntime? { store.fleet.runtime(key.runtimeID) }

    var body: some View {
        Group {
            if let session {
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    content(session, now: context.date)
                }
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
            await feed.load(store.fabricService)
            await command.loadOrders(for: key)
        }
        // the stream's turns, as they finish
        .onChange(of: store.liveTurns[key] ?? [], initial: true) { _, turns in feed.merge(turns) }
        .onChange(of: command.notice) { _, notice in
            guard let notice else { return }
            toast = ToastMessage(text: notice.text, tone: notice.isError ? .error : .neutral)
            command.notice = nil
        }
        .onAppear { store.sessionOnScreen(key) }
        .onDisappear {
            store.sessionOnScreen(nil)
            feed.looked()
        }
        .task(id: scenePhase) {
            // the person opened it (or came back to it) and it had finished:
            // tell the Mac it was seen (FINISHED becomes IDLE). Only on their
            // looking, never because the server reported a state.
            guard scenePhase == .active, session?.state == .finished, let runtime, runtime.online, command.isPaired(with: runtime) else { return }
            try? await Task.sleep(for: .seconds(2))
            guard !Task.isCancelled, scenePhase == .active, session?.state == .finished else { return }
            await command.command(.seen, key)
        }
        .confirmationDialog("Clear the conversation?", isPresented: $confirmClear, titleVisibility: .visible) {
            Button("Clear", role: .destructive) { act { await command.command(.clear, key) } }
        } message: {
            Text("Claude Code's /clear: the session starts again with an empty context.")
        }
    }

    // MARK: content

    private func content(_ s: FleetSession, now: Date) -> some View {
        let header = SessionHeader(s, runtime: runtime, now: now)
        let items = store.fleet.attention(for: key)
        // what each item is comes from what the Mac signed, never the server's kind
        let question = items.first { command.verifiedKind($0) == .question }
        let mode = ComposerMode(s, runtime: runtime)
        return ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    headerCard(header, s)
                    if let runtime, !command.isPaired(with: runtime) { pairNote(runtime) }
                    if runtime?.held == true { heldBanner }
                    if !s.queuePaused.isEmpty { pausedBanner(s) }
                    ForEach(items.filter { $0.id != question?.id }) { item in
                        AttentionCard(item: item, verifiedKind: command.verifiedKind(item), onOpen: { router.approval = AttentionRef(id: item.id) })
                    }
                    if feed.more {
                        Button { Task { await feed.loadOlder(store.fabricService) } } label: {
                            Label(feed.loading ? "Loading\u{2026}" : "Earlier turns", systemImage: "arrow.up.circle")
                                .frame(maxWidth: .infinity)
                        }
                        .buttonStyle(.lapis(.ghost))
                        .accessibilityIdentifier("session.earlier")
                    }
                    if feed.turns.isEmpty, !feed.loading {
                        EmptyCard(icon: "text.bubble", title: feed.loadError ?? "No turns yet",
                                  hint: s.textOff ? "This project has Text off: turns are not kept." : "What it is asked and what it answers shows here, turn by turn.")
                    }
                    ForEach(feed.turns) { turn in
                        if turn.turnID == feed.firstNew, feed.newCount > 0 {
                            newDivider(feed.newCount)
                        }
                        TurnView(row: TurnRow(turn, now: now)).id(turn.turnID)
                    }
                    if s.state == .working || s.state == .stalled { runningNow(s, now: now) }
                    if let question { QuestionCard(item: question, command: command, onDone: done) }
                    ForEach(s.queue, id: \.self) { id in queuedCard(id, s) }
                    ForEach(command.pending.filter { $0.session == key }) { p in UndoCard(pending: p) { draft = command.undo(p.id) ?? draft } }
                    Color.clear.frame(height: 1).id("bottom")
                }
                .padding(.horizontal, Lapis.Layout.gutter)
                .padding(.vertical, 12)
                .frame(maxWidth: Lapis.Layout.readingWidth)
                .frame(maxWidth: .infinity)
            }
            .scrollDismissesKeyboard(.interactively)
            .defaultScrollAnchor(.bottom)
            .onChange(of: feed.turns.count) { proxy.scrollTo("bottom", anchor: .bottom) }
            .onChange(of: command.pending.count) { withAnimation { proxy.scrollTo("bottom", anchor: .bottom) } }
            .safeAreaInset(edge: .bottom) {
                composer(s, mode: mode, question: question)
            }
        }
    }

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
                if let text = order?.text {
                    Button("Edit", systemImage: "pencil") {
                        act {
                            let failure = await command.command(.cancel, key, orderIDs: [id])
                            if failure == nil { draft = text; composing = true }
                            return failure
                        }
                    }
                    .buttonStyle(.lapis(.secondary, size: .small))
                }
                Button("Cancel", systemImage: "xmark") { act { await command.command(.cancel, key, orderIDs: [id]) } }
                    .buttonStyle(.lapis(.destructive, size: .small))
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

    private func pausedBanner(_ s: FleetSession) -> some View {
        HStack(spacing: 10) {
            Image(systemName: "pause.circle.fill").foregroundStyle(Color.lapis.yellowMark)
            Text(FabricWords.queuePaused(s.queuePaused)).font(.subheadline).foregroundStyle(Color.lapis.ink)
            Spacer(minLength: 0)
            Button("Release") { act { await command.command(.releaseQueue, key) } }
                .buttonStyle(.lapis(.secondary, size: .small))
        }
        .padding(12)
        .background(Color.lapis.yellowSoft, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
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
                Button { act { await command.command(.resume, key) } } label: {
                    Label("Resume \(s.address)", systemImage: "play.fill").frame(maxWidth: .infinity)
                }
                .buttonStyle(.lapis(.primary))
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
                        Image(systemName: mode.queues && question == nil ? "text.append" : "arrow.up")
                            .font(.body.weight(.bold))
                            .foregroundStyle(Color.lapis.onAccent)
                            .frame(width: 38, height: 38)
                            .background(Color.lapis.accent, in: .circle)
                    }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityLabel(question != nil ? "Answer" : mode.queues ? "Queue" : "Send")
                    .accessibilityIdentifier("session.send")
                }
                .padding(.horizontal, Lapis.Layout.gutter)
            }
        }
        .padding(.vertical, 10)
        .background(.bar)
    }

    private func send(_ text: String, _ s: FleetSession, _ mode: ComposerMode) {
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
            act {
                let failure = await command.answer(v, words: words)
                if failure == nil { draft = "" }
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
                    Button("Stop", systemImage: "stop.fill") { act { await command.command(.interrupt, key) } }
                        .tint(Color.lapis.red)
                        .accessibilityIdentifier("session.stop")
                }
            }
            ToolbarItem(placement: .topBarTrailing) {
                Menu("More", systemImage: "ellipsis.circle") {
                    Button(s.following ? "Stop following" : "Follow every turn", systemImage: s.following ? "bell.slash" : "bell") {
                        act { await store.follow(key, !s.following) }
                    }
                    Menu("Move to fabric", systemImage: "square.stack.3d.up") {
                        Button("\(s.project.isEmpty ? "Its project" : s.project) (its own)") { act { await command.command(.setFabric, key, text: "") } }
                        ForEach(store.config.fabrics) { f in
                            Button(f.name) { act { await command.command(.setFabric, key, text: f.id) } }
                        }
                    }
                    Divider()
                    if s.state == .asleep || s.state == .exited {
                        Button("Resume", systemImage: "play") { act { await command.command(.resume, key) } }
                    }
                    Button("Compact context", systemImage: "arrow.down.right.and.arrow.up.left") { act { await command.command(.compact, key) } }
                    Button("Clear conversation\u{2026}", systemImage: "trash", role: .destructive) { confirmClear = true }
                    Divider()
                    Button("Copy address", systemImage: "doc.on.doc") { UIPasteboard.general.string = s.address }
                }
                .accessibilityIdentifier("session.more")
            }
        }
    }

    private func act(_ change: @escaping @MainActor () async -> String?) {
        Task {
            if let failure = await change(), !failure.isEmpty { toast = ToastMessage(text: failure, tone: .error) }
        }
    }
}

/// One turn: the prompt as your bubble, what it did in one line, its reply.
struct TurnView: View {
    let row: TurnRow
    @State private var expanded = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if row.textOff {
                Text("Text off: this turn's words are not kept.").font(.footnote).italic().foregroundStyle(Color.lapis.ink3)
            } else if !row.prompt.isEmpty {
                HStack {
                    Spacer(minLength: 40)
                    VStack(alignment: .trailing, spacing: 3) {
                        Text(row.prompt)
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
                let long = row.reply.count > 900
                Text(Self.markdown(long && !expanded ? String(row.reply.prefix(900)) + "\u{2026}" : row.reply))
                    .font(.body)
                    .foregroundStyle(Color.lapis.ink)
                    .textSelection(.enabled)
                    .frame(maxWidth: .infinity, alignment: .leading)
                if long {
                    Button(expanded ? "Show less" : "Show all") { withAnimation { expanded.toggle() } }
                        .buttonStyle(.lapis(.ghost, size: .small))
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

    /// Inline Markdown (bold, code, links), as Claude writes it.
    static func markdown(_ s: String) -> AttributedString {
        (try? AttributedString(markdown: s, options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace))) ?? AttributedString(s)
    }
}

/// A held send, going in a few seconds unless taken back.
struct UndoCard: View {
    let pending: PendingSend
    let onUndo: () -> Void

    var body: some View {
        if pending.lapsed { lapsed } else { counting }
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

/// A sheet's handle on an attention item.
struct AttentionRef: Identifiable, Hashable {
    let id: String
}
