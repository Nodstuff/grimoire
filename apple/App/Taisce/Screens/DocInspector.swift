import SwiftUI
import TaisceKit

/// A doc's inspector (the Lapis inspector artboards): a column beside the
/// doc on iPad and the Mac, a sheet on iPhone. Review (flagged and parked
/// changes, decided here), Comments (the doc's comment threads), History
/// (who wrote what), About (outline, links here, tags, workspace and who
/// shares it, actions).
struct DocInspector: View {
    let docID: DocID
    let surface: SurfaceModel?
    let history: [DocHistoryEntry]
    let page: DocPage?
    /// iPhone: a sheet with its own close button and title
    var asSheet = false
    /// the doc's … menu items
    var menu: AnyView = AnyView(EmptyView())
    var onClose: () -> Void = {}

    enum Tab: String, CaseIterable, Identifiable {
        case review = "Review", comments = "Comments", history = "History", about = "About"
        var id: String { rawValue }
    }

    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @AppStorage("inspector.tab") private var tab: Tab = .review
    @State private var comments: [InspectorComment] = []
    @State private var backlinks: [Backlink]?
    @State private var members: [WorkspaceMember] = []

    private var items: [DocReviewItem] { surface?.reviewItems ?? [] }
    private var title: String { model.index.byID[docID]?.title ?? "" }

    var body: some View {
        VStack(spacing: 14) {
            if asSheet { sheetHeader }
            TabStrip(tab: $tab, reviewCount: items.count, commentCount: comments.count)
                .padding(.horizontal, asSheet ? 16 : 14)
            ScrollView {
                VStack(alignment: .leading, spacing: 12) {
                    switch tab {
                    case .review: reviewTab
                    case .comments: commentsTab
                    case .history: historyTab
                    case .about: aboutTab
                    }
                }
                .padding(.horizontal, asSheet ? 16 : 14)
                .padding(.bottom, 24)
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .scrollBounceBehavior(.basedOnSize)
        }
        .padding(.top, asSheet ? 6 : 10)
        .background((asSheet ? Color.lapis.bg : Color.lapis.panel).ignoresSafeArea())
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationBackground(Color.lapis.bg)
        .task(id: docID) { await loadComments() }
        .task(id: tab) { if tab == .about { await loadAbout() } }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(title) inspector")
    }

    // MARK: header (iPhone sheet)

    private var sheetHeader: some View {
        HStack(spacing: 8) {
            Button(action: onClose) {
                Image(systemName: "xmark").font(.body.weight(.semibold))
                    .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                    .contentShape(.circle)
            }
            .buttonStyle(.plain)
            .glassEffect(.regular.interactive(), in: .circle)
            .accessibilityLabel("Close")
            VStack(spacing: 1) {
                Text(title).font(.headline).foregroundStyle(Color.lapis.ink).lineLimit(1)
                Text(subtitle).font(.caption).foregroundStyle(Color.lapis.ink3).lineLimit(1).truncationMode(.head)
            }
            .frame(maxWidth: .infinity)
            .accessibilityElement(children: .combine)
            Menu { menu } label: {
                Image(systemName: "ellipsis").font(.body.weight(.semibold))
                    .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                    .contentShape(.circle)
            }
            .buttonStyle(.plain)
            .glassEffect(.regular.interactive(), in: .circle)
            .accessibilityLabel("More")
        }
        .foregroundStyle(Color.lapis.ink)
        .padding(.horizontal, 16)
    }

    /// "Work › Taisce · v14"
    private var subtitle: String {
        var parts = [model.workspaceBadge(for: docID)?.name].compactMap { $0 } + model.index.ancestors(of: docID).map(\.title)
        if parts.isEmpty { parts = ["Top level"] }
        let crumb = parts.joined(separator: DocIndex.separator)
        let v = model.index.byID[docID]?.currentEpoch ?? 0
        return v > 0 ? "\(crumb) · v\(v)" : crumb
    }

    // MARK: Review

    @ViewBuilder private var reviewTab: some View {
        if !items.isEmpty {
            SectionLabel("Wants a look")
            ForEach(items) { item in
                ReviewItemCard(item: item, canDecide: surface?.editable ?? false, busy: surface?.deciding.contains(item.id) ?? false,
                               onDecide: { d in Task { await surface?.decide(item, d) } },
                               onShow: item.block.map { b in { show(b) } })
            }
        } else if surface?.reviewLoaded == true || surface == nil {
            EmptyCard(icon: "checkmark.circle", title: "Nothing waiting", hint: "Flagged and parked changes to this doc show here, to keep or undo.")
        } else {
            SkeletonBlock(height: 120).shimmering()
        }
        let recent = Array(HistoryGroup.make(history).filter(\.applied).prefix(4))
        if !recent.isEmpty {
            SectionLabel("Earlier").padding(.top, 8)
            HistoryList(groups: recent)
        }
    }

    // MARK: Comments

    @ViewBuilder private var commentsTab: some View {
        if comments.isEmpty {
            EmptyCard(icon: "bubble.left", title: "No comments", hint: "Comments people and agents leave on this doc's blocks show here.")
        } else {
            ForEach(comments) { c in
                VStack(alignment: .leading, spacing: 8) {
                    if let on = c.on {
                        Text("On \u{201C}\(on)\u{201D}").lapisFont(.label).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                    }
                    HStack(spacing: 8) {
                        LapisAvatar(name: c.author, size: 26)
                        Text(c.author)
                            .font(Principal.isAgent(c.author) ? .lapisMono(.subheadline, weight: .medium) : .subheadline.weight(.semibold))
                            .foregroundStyle(Principal.isAgent(c.author) ? Color.lapis.agent : Color.lapis.ink)
                            .lineLimit(1)
                        if let d = c.date {
                            Text("· \(ShortWhen.string(d))").font(.subheadline).foregroundStyle(Color.lapis.ink3)
                        }
                    }
                    Text(ReviewText.plain(c.text)).font(.subheadline).foregroundStyle(Color.lapis.ink).fixedSize(horizontal: false, vertical: true)
                }
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
                .accessibilityElement(children: .combine)
                .onTapGesture { if let b = c.refersTo { show(b) } }
            }
        }
    }

    // MARK: History

    @ViewBuilder private var historyTab: some View {
        let groups = HistoryGroup.make(history)
        if groups.isEmpty {
            EmptyCard(icon: "clock", title: "No history yet", hint: model.isOnline ? "Edits to this doc show here." : "History shows when you're online.")
        } else {
            HistoryList(groups: groups)
        }
    }

    // MARK: About

    @ViewBuilder private var aboutTab: some View {
        let outline = surface.map { s in OutlineEntry.make(Surface.blocks(s.textView.textStorage).map { ($0.id, $0.content.markdown) }) } ?? []
        if !outline.isEmpty {
            SectionLabel("Outline")
            VStack(alignment: .leading, spacing: 0) {
                ForEach(outline) { h in
                    Button { show(h.id) } label: {
                        Text(h.title)
                            .font(h.level <= 1 ? .subheadline.weight(.semibold) : .subheadline)
                            .foregroundStyle(h.level <= 2 ? Color.lapis.ink : Color.lapis.ink2)
                            .lineLimit(2)
                            .padding(.leading, CGFloat(max(0, h.level - 1)) * 14)
                            .frame(maxWidth: .infinity, minHeight: Lapis.Layout.minTarget, alignment: .leading)
                            .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityLabel("\(h.title), heading level \(h.level)")
                    .accessibilityHint("Shows it in the doc")
                }
            }
            .padding(.horizontal, 14)
            .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
        }
        SectionLabel("Links here").padding(.top, outline.isEmpty ? 0 : 8)
        if let backlinks {
            if backlinks.isEmpty {
                Text("No other doc links to this one.").font(.subheadline).foregroundStyle(Color.lapis.ink3).padding(.horizontal, 4)
            } else {
                VStack(spacing: 0) {
                    ForEach(Array(backlinks.enumerated()), id: \.element.id) { i, link in
                        Button { open(link.docID) } label: {
                            HStack(alignment: .top, spacing: 10) {
                                Image(systemName: "doc.text").foregroundStyle(Color.lapis.ink3).frame(width: 22).accessibilityHidden(true)
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(link.docTitle).font(.subheadline.weight(.semibold)).foregroundStyle(Color.lapis.ink).lineLimit(1)
                                    Text(ReviewText.plain(link.content)).font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(2)
                                }
                                Spacer(minLength: 0)
                            }
                            .padding(.vertical, 10)
                            .frame(minHeight: Lapis.Layout.minTarget)
                            .contentShape(.rect)
                        }
                        .buttonStyle(.plain)
                        .overlay(alignment: .top) { if i > 0 { Rectangle().fill(Color.lapis.line2).frame(height: 1).padding(.leading, 32) } }
                    }
                }
                .padding(.horizontal, 14)
                .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
            }
        } else {
            SkeletonLines(count: 2)
        }
        if let page, page.status != nil || !page.tags.isEmpty {
            SectionLabel("Tags").padding(.top, 8)
            FlowRow(spacing: 6) {
                if let s = page.status { DocPill(text: s, dot: DocPill.statusColor(s)) }
                ForEach(page.tags, id: \.self) { tag in
                    DocPill(text: "#\(tag)")
                }
            }
        }
        if let badge = model.workspaceBadge(for: docID) {
            SectionLabel("Workspace").padding(.top, 8)
            VStack(alignment: .leading, spacing: 0) {
                HStack(spacing: 10) {
                    WorkspaceDot(color: badge.color, size: 10)
                    Text(badge.name).font(.subheadline.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                    Spacer()
                    Text(model.access(for: docID).note ?? (badge.shared ? "Shared" : "Only you"))
                        .font(.footnote).foregroundStyle(Color.lapis.ink3)
                }
                .frame(minHeight: Lapis.Layout.minTarget)
                ForEach(members) { m in
                    HStack(spacing: 10) {
                        LapisAvatar(name: m.name, agent: false, size: 26)
                        Text(m.name).font(.subheadline).foregroundStyle(Color.lapis.ink)
                        Spacer()
                        Text(m.role.capitalized).font(.footnote).foregroundStyle(Color.lapis.ink3)
                    }
                    .frame(minHeight: Lapis.Layout.minTarget)
                    .overlay(alignment: .top) { Rectangle().fill(Color.lapis.line2).frame(height: 1).padding(.leading, 36) }
                    .accessibilityElement(children: .combine)
                }
            }
            .padding(.horizontal, 14)
            .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
            .accessibilityElement(children: .contain)
        }
        SectionLabel("Actions").padding(.top, 8)
        VStack(alignment: .leading, spacing: 0) {
            menu
        }
        .buttonStyle(InspectorActionStyle())
        .labelStyle(.titleAndIcon)
        .padding(.horizontal, 14)
        .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
    }

    // MARK: actions

    private func show(_ block: BlockID) {
        if asSheet { onClose() }
        Task { @MainActor in
            if asSheet { try? await Task.sleep(for: .milliseconds(350)) }
            surface?.scrollTo(block: block)
        }
    }

    private func open(_ doc: DocID) {
        if asSheet { onClose() }
        router.open(.doc(doc))
    }

    private func loadComments() async {
        guard let cache = model.cache, let records = try? await cache.blocks(of: docID) else { return }
        let blocks = records.map(\.block)
        let byID = Dictionary(blocks.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
        let authors = Dictionary(history.compactMap { row in row.targetBlock.map { ($0, row) } }, uniquingKeysWith: { _, older in older })
        comments = blocks.filter { $0.blockType == .comment && !$0.deleted }.map { b in
            let row = authors[b.id]
            let on = b.refersTo.flatMap { byID[$0] }.map { ReviewText.plain($0.content).components(separatedBy: "\n").first ?? "" }
            return InspectorComment(id: b.id, author: row.map { $0.principalIsYours == true && $0.principalKind == "human" ? "You" : $0.principalName } ?? "someone",
                                    date: row?.date, text: b.content, refersTo: b.refersTo,
                                    on: on.map { $0.count > 40 ? String($0.prefix(40)) + "\u{2026}" : $0 })
        }
    }

    private func loadAbout() async {
        guard let api = model.api else { backlinks = backlinks ?? []; return }
        if backlinks == nil || model.isOnline { backlinks = (try? await api.backlinks(docID)) ?? backlinks ?? [] }
        if let ws = model.index.byID[docID]?.workspaceID, model.workspacePicker.isShared(.id(ws)) {
            members = (try? await api.workspaceMembers(ws)) ?? members
        }
    }
}

/// A comment block as the inspector lists it.
struct InspectorComment: Identifiable, Hashable {
    var id: BlockID
    var author: String
    var date: Date?
    var text: String
    var refersTo: BlockID?
    /// the first words of the block it is about
    var on: String?
}

/// Review · Comments · History · About, with counts (the artboards' segmented strip).
private struct TabStrip: View {
    @Binding var tab: DocInspector.Tab
    let reviewCount: Int
    let commentCount: Int
    @Namespace private var selection

    var body: some View {
        HStack(spacing: 0) {
            ForEach(DocInspector.Tab.allCases) { t in
                Button {
                    withAnimation(Lapis.Motion.standard) { tab = t }
                } label: {
                    HStack(spacing: 5) {
                        Text(t.rawValue).font(.subheadline.weight(tab == t ? .semibold : .medium)).lineLimit(1).minimumScaleFactor(0.75)
                        if t == .review, reviewCount > 0 { CountPill(n: reviewCount, tone: .attention) }
                        if t == .comments, commentCount > 0 { CountPill(n: commentCount) }
                    }
                    .foregroundStyle(Color.lapis.ink)
                    .frame(maxWidth: .infinity, minHeight: 34)
                    .background {
                        if tab == t {
                            Capsule().fill(Color.lapis.card).shadow(color: .black.opacity(0.1), radius: 4, y: 2)
                                .matchedGeometryEffect(id: "tab", in: selection)
                        }
                    }
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityAddTraits(tab == t ? [.isSelected] : [])
                .accessibilityLabel(t == .review && reviewCount > 0 ? "Review, \(reviewCount) waiting" : t.rawValue)
            }
        }
        .padding(3)
        .background(Color.lapis.hover, in: .capsule)
        .frame(minHeight: Lapis.Layout.minTarget)
        .sensoryFeedback(.selection, trigger: tab)
    }
}

/// A flagged or parked change in the inspector: who, what, the change in
/// words, why, and the decision.
struct ReviewItemCard: View {
    let item: DocReviewItem
    let canDecide: Bool
    var busy = false
    let onDecide: (DocReviewDecision) -> Void
    var onShow: (() -> Void)?

    @State private var confirming = false

    var body: some View {
        let card = ChangeCardModel(item)
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 10) {
                LapisAvatar(name: card.author, size: 32)
                VStack(alignment: .leading, spacing: 1) {
                    Text(card.author)
                        .font(item.byAgent ? .lapisMono(.subheadline, weight: .medium) : .subheadline.weight(.semibold))
                        .foregroundStyle(item.byAgent ? Color.lapis.agent : Color.lapis.ink)
                        .lineLimit(1)
                    Text([item.summary, card.date.map { ShortWhen.string($0) }].compactMap { $0 }.joined(separator: " · "))
                        .font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                }
                Spacer(minLength: 4)
                VerdictChip(card.verdict, short: card.verdict == .red)
            }
            if item.opType != "delete", !(card.old.isEmpty && card.new.isEmpty) {
                DiffText(old: card.old, new: card.new)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(card.verdict.soft, in: .rect(cornerRadius: 14, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 14, style: .continuous).strokeBorder(card.verdict.mark, lineWidth: 1))
            }
            if let note = card.note {
                Text(note).font(.subheadline).foregroundStyle(Color.lapis.ink2).fixedSize(horizontal: false, vertical: true)
            }
            if canDecide {
                HStack(spacing: 8) {
                    let delete = item.kind == .parked && item.opType == "delete"
                    Button {
                        if delete, !confirming { Haptics.warning(); confirming = true; return }
                        confirming = false
                        Haptics.success()
                        onDecide(.accept)
                    } label: {
                        Label(confirming ? "Confirm delete" : card.acceptTitle, systemImage: delete ? "trash" : "checkmark")
                    }
                    .buttonStyle(CardActionStyle(kind: card.verdict == .yellow ? .keep : (delete ? .danger : .apply)))
                    Button {
                        confirming = false
                        Haptics.impact()
                        onDecide(.decline)
                    } label: {
                        Text(card.rejectTitle)
                    }
                    .buttonStyle(CardActionStyle(kind: .outline))
                    if let onShow {
                        Button(action: onShow) {
                            Image(systemName: "arrow.up.forward.square").font(.body)
                                .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                                .background(Color.lapis.hover, in: .circle)
                                .contentShape(.circle)
                        }
                        .buttonStyle(.plain)
                        .foregroundStyle(Color.lapis.ink)
                        .accessibilityLabel("Show in doc")
                    }
                }
                .disabled(busy)
                .task(id: confirming) {
                    guard confirming else { return }
                    try? await Task.sleep(for: .seconds(4))
                    confirming = false
                }
            }
        }
        .padding(14)
        .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(card.verdict.spoken) change by \(card.author): \(item.summary)")
    }
}

/// Old words struck in red, new ones underlined in green (Newsreader).
struct DiffText: View {
    let old: String
    let new: String

    var body: some View {
        let segments = WordDiff.display(old: old, new: new)
        var out = AttributedString()
        for seg in segments {
            var part = AttributedString(seg.text)
            switch seg.kind {
            case .same: break
            case .removed:
                part.strikethroughStyle = .single
                part.foregroundColor = Color.lapis.red
                part.backgroundColor = Color.lapis.redSoft
            case .added:
                part.underlineStyle = .thick
                part.underlineColor = UIColor.lapis.green
                part.backgroundColor = Color.lapis.greenSoft
            }
            out += part
        }
        let removed = segments.filter { $0.kind == .removed }.map(\.text).joined(separator: " … ")
        let added = segments.filter { $0.kind == .added }.map(\.text).joined(separator: " … ")
        return Text(out)
            .lapisFont(.docBody)
            .foregroundStyle(Color.lapis.ink)
            .fixedSize(horizontal: false, vertical: true)
            .accessibilityLabel([removed.isEmpty ? nil : "Removed: \(removed)", added.isEmpty ? nil : "Added: \(added)"].compactMap { $0 }.joined(separator: ". "))
    }
}

/// History rows in a card: avatar, who and what, when, the verdict.
struct HistoryList: View {
    let groups: [HistoryGroup]

    var body: some View {
        VStack(spacing: 0) {
            ForEach(Array(groups.enumerated()), id: \.element.id) { i, g in
                HStack(spacing: 12) {
                    LapisAvatar(name: g.who == "You" ? "You" : g.who, agent: g.isAgent, size: 32)
                    VStack(alignment: .leading, spacing: 1) {
                        if g.isAgent {
                            Text(g.who).font(.lapisMono(.subheadline, weight: .medium)).foregroundStyle(Color.lapis.agent).lineLimit(1)
                        } else {
                            Text(g.count > 1 ? "\(g.who) · \(g.count) edits" : g.who).font(.subheadline).foregroundStyle(Color.lapis.ink).lineLimit(1)
                        }
                        Text([g.count > 1 && !g.isAgent ? nil : g.summary, g.date.map { ShortWhen.string($0) }].compactMap { $0 }.joined(separator: " · "))
                            .font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                    }
                    Spacer(minLength: 4)
                    VerdictChip(g.applied ? .green : .red, short: true)
                }
                .padding(.vertical, 8)
                .frame(minHeight: 56)
                .overlay(alignment: .top) { if i > 0 { Rectangle().fill(Color.lapis.line2).frame(height: 1).padding(.leading, 44) } }
                .accessibilityElement(children: .combine)
            }
        }
        .padding(.horizontal, 14)
        .background(Color.lapis.card, in: .rect(cornerRadius: 20, style: .continuous))
    }
}

/// The About tab's action rows: full-width, 44 pt, a hairline between.
private struct InspectorActionStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.subheadline)
            .foregroundStyle(configuration.role == .destructive ? Color.lapis.red : Color.lapis.ink)
            .frame(maxWidth: .infinity, minHeight: Lapis.Layout.minTarget, alignment: .leading)
            .contentShape(.rect)
            .opacity(configuration.isPressed ? 0.6 : 1)
    }
}
