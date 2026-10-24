import SwiftUI
import TaisceKit

/// Every change waiting for a person, across the docs you can see: flagged
/// (applied; keep or revert) and parked (not applied; apply or discard).
/// Tap a row's doc to read it in place; decide here or in the doc.
struct ReviewScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var toast: ToastMessage?

    var body: some View {
        let home = model.home
        ReviewContent(
            items: home.review,
            error: home.reviewError,
            resolving: home.resolving,
            onOpen: { router.open(.doc($0)) },
            onDecide: decide
        )
        .refreshable { await home.loadReview() }
        .task(id: model.cursor) { await home.loadReview() }
        .lapisToast($toast)
    }

    private func decide(_ item: ReviewQueueItem, _ decision: ReviewQueueDecision) {
        Task {
            if let failure = await model.home.resolve(item, decision) {
                toast = ToastMessage(text: failure, tone: .error)
            } else {
                let (accept, decline) = ReviewCopy.actions(item.kind)
                let word = decision == .accept ? accept : decline
                toast = ToastMessage(text: "\(ReviewCopy.pastTense(word)) \u{00B7} \(item.docTitle)", tone: .success)
            }
        }
    }
}

extension ReviewCopy {
    /// "Kept", "Reverted", "Applied", "Discarded"
    static func pastTense(_ action: String) -> String {
        switch action {
        case "Keep": "Kept"
        case "Revert": "Reverted"
        case "Apply": "Applied"
        case "Discard": "Discarded"
        default: action
        }
    }
}

struct ReviewContent: View {
    /// nil while loading
    let items: [ReviewQueueItem]?
    var error: String?
    var resolving: Set<String> = []
    var onOpen: (DocID) -> Void = { _ in }
    var onDecide: (ReviewQueueItem, ReviewQueueDecision) -> Void = { _, _ in }
    @State private var confirmingApply: ReviewQueueItem?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                VStack(alignment: .leading, spacing: 4) {
                    Text("Review")
                        .lapisFont(.display)
                        .foregroundStyle(Color.lapis.ink)
                        .accessibilityAddTraits(.isHeader)
                    if let items, !items.isEmpty {
                        Text(ReviewCopy.subtitle(ReviewQueueSummary(items))).font(.subheadline).foregroundStyle(Color.lapis.ink3)
                    }
                }
                .padding(.horizontal, 4)
                if let error, items == nil {
                    LapisBanner(text: "Couldn't load the review queue. \(error)", tone: .yellow, symbol: "exclamationmark.triangle")
                }
                if let items {
                    if items.isEmpty {
                        EmptyState(symbol: "checkmark.seal", title: "Nothing to review", message: "Changes an agent makes that want a person's eye wait here.")
                    } else {
                        ForEach(group(items), id: \.title) { g in
                            VStack(alignment: .leading, spacing: 8) {
                                HStack(spacing: 6) {
                                    Text(g.title).foregroundStyle(g.kind == .flagged ? Color.lapis.yellow : Color.lapis.red)
                                    Text("\(g.items.count)").foregroundStyle(Color.lapis.ink3).monospacedDigit()
                                }
                                .font(.headline)
                                .accessibilityElement(children: .combine)
                                .accessibilityAddTraits(.isHeader)
                                .padding(.horizontal, 4)
                                ForEach(g.items) { item in
                                    ReviewItemCard(
                                        item: item, busy: resolving.contains(item.id),
                                        onOpen: { onOpen(item.docID) },
                                        onAccept: {
                                            if ReviewCopy.acceptIsDestructive(item) { confirmingApply = item } else { onDecide(item, .accept) }
                                        },
                                        onDecline: { onDecide(item, .decline) }
                                    )
                                    .transition(.asymmetric(insertion: .opacity, removal: .move(edge: .leading).combined(with: .opacity)))
                                }
                            }
                        }
                    }
                } else if error == nil {
                    SkeletonBlock(height: 140, radius: Lapis.Radius.tab)
                    SkeletonBlock(height: 140, radius: Lapis.Radius.tab)
                }
            }
            .padding(.horizontal, 16)
            .padding(.top, 4)
            .padding(.bottom, 32)
            .frame(maxWidth: Lapis.Layout.readingWidth)
            .frame(maxWidth: .infinity)
            .animation(Lapis.Motion.standard, value: items?.map(\.id))
        }
        .groundBackground()
        .navigationTitle("Review")
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog(
            confirmingApply.map { "\($0.summary)?" } ?? "",
            isPresented: Binding(get: { confirmingApply != nil }, set: { if !$0 { confirmingApply = nil } }),
            titleVisibility: .visible, presenting: confirmingApply
        ) { item in
            Button("Apply", role: .destructive) { onDecide(item, .accept) }
        } message: { item in
            Text("\(item.proposer) asked for this. The doc can be restored from the Trash on the web.")
        }
    }

    private struct Group { var title: String; var kind: ReviewQueueItem.Kind; var items: [ReviewQueueItem] }

    private func group(_ items: [ReviewQueueItem]) -> [Group] {
        [
            Group(title: "Flagged", kind: .flagged, items: items.filter { $0.kind == .flagged }),
            Group(title: "Parked", kind: .parked, items: items.filter { $0.kind == .parked }),
        ].filter { !$0.items.isEmpty }
    }
}

/// One queued change: who, what, the text, and the two decisions.
struct ReviewItemCard: View {
    let item: ReviewQueueItem
    var busy = false
    var onOpen: () -> Void = {}
    var onAccept: () -> Void = {}
    var onDecline: () -> Void = {}

    var body: some View {
        let verdict = ReviewCopy.verdict(item.kind)
        let (accept, decline) = ReviewCopy.actions(item.kind)
        VStack(alignment: .leading, spacing: 10) {
            // the badge and the chip side by side, or stacked when that won't fit (large text)
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 8) {
                    AgentBadge(name: item.proposer).fixedSize()
                    Spacer(minLength: 8)
                    VerdictChip(verdict, short: true)
                }
                VStack(alignment: .leading, spacing: 6) {
                    AgentBadge(name: item.proposer)
                    VerdictChip(verdict, short: true)
                }
            }
            Button(action: onOpen) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(item.docTitle.isEmpty ? "Untitled" : item.docTitle)
                        .lapisFont(.cardTitle)
                        .foregroundStyle(Color.lapis.ink)
                    Text(item.summary).font(.subheadline).foregroundStyle(Color.lapis.ink2)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .contentShape(.rect)
            }
            .buttonStyle(.plain)
            .accessibilityHint("Opens the doc")
            if let text = excerpt {
                Text(text)
                    .lapisFont(.docBody)
                    .foregroundStyle(Color.lapis.ink)
                    .lineLimit(4)
                    .padding(10)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(verdict.soft, in: .rect(cornerRadius: Lapis.Radius.md, style: .continuous))
                    .accessibilityLabel("Proposed text: \(text)")
            }
            HStack(spacing: 10) {
                Button(ReviewCopy.acceptIsDestructive(item) ? "\(accept)\u{2026}" : accept, action: onAccept)
                    .buttonStyle(.lapis(item.kind == .flagged ? .keep : .primary, fullWidth: true))
                Button(decline, action: onDecline)
                    .buttonStyle(.lapis(item.kind == .flagged ? .revert : .secondary, fullWidth: true))
            }
            .disabled(busy)
            .overlay { if busy { ProgressView().tint(Color.lapis.ink3) } }
        }
        .padding(16)
        .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.tab, style: .continuous))
        .overlay {
            if item.kind == .flagged {
                RoundedRectangle(cornerRadius: Lapis.Radius.tab, style: .continuous).strokeBorder(Color.lapis.yellowMark.opacity(0.6), lineWidth: 1.5)
            }
        }
        .accessibilityElement(children: .contain)
    }

    /// The proposed text of an edit, as plain words.
    private var excerpt: String? {
        guard let p = item.proposed?.trimmingCharacters(in: .whitespacesAndNewlines), !p.isEmpty else { return nil }
        return p
    }
}

#if DEBUG
#Preview("Review") {
    NavigationStack { ReviewContent(items: PreviewData.reviewItems) }.preferredColorScheme(.light)
}

#Preview("Review, dark") {
    NavigationStack { ReviewContent(items: PreviewData.reviewItems) }.preferredColorScheme(.dark)
}

#Preview("Review, empty") {
    NavigationStack { ReviewContent(items: []) }.preferredColorScheme(.light)
}
#endif
