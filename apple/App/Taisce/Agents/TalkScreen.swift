import SwiftUI
import TaisceKit

/// Agents talking to each other (Slice 5): who asked whom, when, whether it
/// was answered, and the text of any message not yet read (a body is
/// deleted once its receiver reads it). At the top, what only you can
/// settle: a pair that reached its limit, mail for a session kept asleep.
struct TalkScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var toast: ToastMessage?

    private var talk: TalkStore { model.fleet.talk }

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            List {
                Section {
                    Text("Agents asking each other, across accounts. Text is deleted once read.")
                        .font(.subheadline).foregroundStyle(Color.lapis.ink3)
                        .listRowBackground(Color.clear)
                        .listRowInsets(EdgeInsets(top: 4, leading: 6, bottom: 4, trailing: 6))
                }
                if !talk.items.isEmpty {
                    Section {
                        ForEach(talk.items) { item in
                            TalkItemCard(item: item, now: context.date, act: act)
                                .listRowInsets(EdgeInsets())
                                .listRowBackground(Color.lapis.card)
                        }
                    } header: { SectionLabel("Needs you") }
                }
                Section {
                    if talk.conversations.isEmpty {
                        EmptyCard(icon: "bubble.left.and.bubble.right", title: talk.loadError ?? (talk.loaded ? "No conversations yet" : "Loading\u{2026}"),
                                  hint: "When one session asks another something with the fabric's send tool, it shows here.")
                            .listRowInsets(EdgeInsets())
                            .listRowBackground(Color.clear)
                            .accessibilityIdentifier("talk.empty")
                    }
                    ForEach(talk.conversations) { c in
                        NavigationLink(value: Route.conversation(c.id)) {
                            ConversationRow(c: c, now: context.date)
                        }
                        .listRowBackground(Color.lapis.card)
                        .accessibilityIdentifier("talk.row.\(c.a.address)~\(c.b.address)")
                    }
                } header: { if !talk.conversations.isEmpty { SectionLabel("Conversations") } }
            }
            .listStyle(.insetGrouped)
            .scrollContentBackground(.hidden)
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
        }
        .groundBackground()
        .navigationTitle("Talk")
        .navigationBarTitleDisplayMode(.inline)
        .refreshable { await talk.load() }
        .task { await talk.load() }
        .lapisToast($toast)
    }

    private func act(_ change: @escaping @MainActor () async -> String?) {
        Task { if let f = await change(), !f.isEmpty { toast = ToastMessage(text: f, tone: .error) } }
    }
}

/// "workbox/main → taisce/main", how many, when, answered or waiting.
struct ConversationRow: View {
    let c: FabricConversation
    let now: Date

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(c.a.address).font(.system(.subheadline, design: .monospaced).weight(.medium))
                Image(systemName: "arrow.left.arrow.right").font(.caption).foregroundStyle(Color.lapis.ink3)
                Text(c.b.address).font(.system(.subheadline, design: .monospaced).weight(.medium))
            }
            .foregroundStyle(Color.lapis.ink)
            .lineLimit(1)
            .truncationMode(.middle)
            FlowRow(spacing: 6) {
                if c.paused { StatusChip(text: "Paused at the limit", tone: .yellow, symbol: "pause.fill") }
                if c.unansweredAsks > 0 {
                    StatusChip(text: c.unansweredAsks == 1 ? "1 ask waiting" : "\(c.unansweredAsks) asks waiting", tone: .agent)
                } else if c.total > 0 {
                    StatusChip(text: "Answered", tone: .green, symbol: "checkmark")
                }
                Text("\(c.total) messages \u{00B7} \(FleetTime.elapsed(since: c.lastAt, now: now))")
                    .font(.caption).foregroundStyle(Color.lapis.ink3)
            }
        }
        .padding(.vertical, 6)
        .accessibilityElement(children: .combine)
    }
}

/// A hub item: a pair paused at its limit (continue or stop), or mail for a
/// session that the cold rule or sleep keeps from answering (wake it, or let it wait).
struct TalkItemCard: View {
    let item: FabricAttentionItem
    let now: Date
    let act: (@escaping @MainActor () async -> String?) -> Void
    @Environment(AppModel.self) private var model

    var body: some View {
        let talk = model.fleet.talk
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                if let from = item.fromAddress, !from.isEmpty {
                    Text(from).font(.system(.subheadline, design: .monospaced).weight(.medium))
                    Image(systemName: "arrow.right").font(.caption).foregroundStyle(Color.lapis.ink3)
                }
                Text(item.address).font(.system(.subheadline, design: .monospaced).weight(.medium))
                if let s = model.fleet.fleet.session(item.session), s.state.isResting {
                    StatusChip(text: s.state.words.lowercased(), tone: .neutral)
                }
            }
            .foregroundStyle(Color.lapis.ink)
            .lineLimit(1)
            if !item.text.isEmpty {
                Text(item.text).font(.body).foregroundStyle(Color.lapis.ink)
            }
            Text(explanation).font(.footnote).foregroundStyle(Color.lapis.ink3)
            HStack {
                switch item.kind {
                case .pairPaused:
                    Button("Let them continue") { act { await talk.continuePair(item.conversationID ?? "") } }
                        .buttonStyle(.lapis(.primary, size: .small))
                        .accessibilityIdentifier("talk.continue")
                    Button("Stop them") { act { await talk.stopPair(item.conversationID ?? "") } }
                        .buttonStyle(.lapis(.destructive, size: .small))
                        .accessibilityIdentifier("talk.stop")
                default:
                    Button(wakeWord) { act { await talk.wake(item) } }
                        .buttonStyle(.lapis(.primary, size: .small))
                        .accessibilityIdentifier("talk.wake")
                    Button("Let it wait") { act { await talk.dismiss(item) } }
                        .buttonStyle(.lapis(.secondary, size: .small))
                        .accessibilityIdentifier("talk.wait")
                }
            }
        }
        .padding(14)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("talk.item.\(item.kind.rawValue)")
    }

    private var wakeWord: String {
        let s = model.fleet.fleet.session(item.session)
        return s?.state == .asleep || s?.state == .exited ? "Resume to answer" : "Wake to answer"
    }

    private var explanation: String {
        let s = model.fleet.fleet.session(item.session)
        let account = s?.account.isEmpty == false ? " on the \(s!.account) account" : ""
        switch item.kind {
        case .pairPaused:
            let limit = model.fleet.config.settings.limits?.effective.pairMessages ?? FabricLimits.defaults.pairMessages
            return "They have sent each other \(limit) messages since you last spoke to either. New ones are held until you choose."
        case .askWaiting:
            return "\(item.address) is \(s?.state.words.lowercased() ?? "asleep"). Resuming it costs a turn\(account)."
        default:
            return "\(item.address) is idle with a full context, so it wasn't woken. Waking it costs a turn\(account)."
        }
    }
}

/// One conversation's messages, oldest at the top.
struct ConversationScreen: View {
    let id: String
    @Environment(AppModel.self) private var model

    private var talk: TalkStore { model.fleet.talk }

    var body: some View {
        let c = talk.conversation(id)
        let messages = Array((talk.messages[id] ?? []).reversed())
        TimelineView(.periodic(from: .now, by: 30)) { context in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 12) {
                    if let c, c.paused {
                        LapisBanner(text: "Paused at the pair limit: new messages are held.", tone: .yellow, symbol: "pause.fill")
                    }
                    if messages.isEmpty {
                        EmptyCard(icon: "bubble.left.and.bubble.right", title: "No messages kept", hint: "Messages are kept for a week; their text only until read.")
                    }
                    ForEach(messages) { m in
                        MessageView(m: m, leading: m.from.sessionID == c?.a.sessionID, now: context.date)
                    }
                }
                .padding(Lapis.Layout.gutter)
                .frame(maxWidth: Lapis.Layout.readingWidth)
                .frame(maxWidth: .infinity)
            }
            .defaultScrollAnchor(.bottom)
        }
        .groundBackground()
        .navigationTitle(c.map { "\($0.a.address) \u{2194} \($0.b.address)" } ?? "Conversation")
        .navigationBarTitleDisplayMode(.inline)
        .task { await talk.open(id) }
        .refreshable { await talk.open(id) }
    }
}

struct MessageView: View {
    let m: FabricMessage
    /// the conversation's first party speaks from the leading side
    let leading: Bool
    let now: Date

    var body: some View {
        VStack(alignment: leading ? .leading : .trailing, spacing: 4) {
            HStack(spacing: 6) {
                Text(m.from.address).font(.system(.caption, design: .monospaced).weight(.semibold)).foregroundStyle(Color.lapis.agent)
                StatusChip(text: FabricWords.messageKind(m.kind), tone: m.kind == .bounce ? .red : m.kind == .ask ? .agent : .neutral)
            }
            Group {
                if let body = m.body {
                    Text(body).foregroundStyle(Color.lapis.ink)
                } else {
                    Text(m.state == .bounced ? "Bounced\(m.bounceReason.map { ": \($0)" } ?? "")" : "Read \u{00B7} text deleted")
                        .italic().foregroundStyle(Color.lapis.ink3)
                }
            }
            .font(.body)
            .padding(12)
            .background(leading ? Color.lapis.card : Color.lapis.agentSoft, in: .rect(cornerRadius: 16, style: .continuous))
            Text(status).font(.caption2).foregroundStyle(Color.lapis.ink3)
        }
        .frame(maxWidth: .infinity, alignment: leading ? .leading : .trailing)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("message.\(m.id)")
    }

    private var status: String {
        var parts = [FleetTime.clock(m.sentAt, now: now)]
        if m.kind == .ask { parts.append(m.answered ? "answered" : "waiting for a reply") }
        if let read = m.readAt { parts.append("read \(FleetTime.elapsed(since: read, now: now)) ago".replacingOccurrences(of: "now ago", with: "just now")) }
        if m.state == .held { parts.append("held") }
        return parts.joined(separator: " \u{00B7} ")
    }
}
