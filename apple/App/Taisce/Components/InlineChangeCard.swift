import SwiftUI
import TaisceKit

/// Word-level diff for showing a change inline: old words struck, new
/// words underlined, the rest as is. Pure (tested in `LapisTests`).
enum WordDiff {
    enum Kind: Hashable, Sendable { case same, removed, added }

    struct Segment: Hashable, Sendable {
        var kind: Kind
        var text: String
    }

    /// Splits into words with their trailing spaces, so joining the
    /// segments of either side gives back exactly that side.
    static func tokens(_ s: String) -> [String] {
        var out: [String] = []
        var cur = ""
        var inSpace = false
        for ch in s {
            let space = ch.isWhitespace
            if !space && inSpace && !cur.isEmpty {
                out.append(cur)
                cur = ""
            }
            cur.append(ch)
            inSpace = space
        }
        if !cur.isEmpty { out.append(cur) }
        return out
    }

    /// The diff of `old` → `new` by words (LCS; removals before additions
    /// at each change; neighbouring segments of one kind merged).
    static func segments(old: String, new: String) -> [Segment] {
        let a = tokens(old), b = tokens(new)
        // too big for an O(n·m) table: show it as a whole replacement
        guard a.count * b.count <= 250_000 else {
            return [Segment(kind: .removed, text: old), Segment(kind: .added, text: new)].filter { !$0.text.isEmpty }
        }
        var lcs = Array(repeating: Array(repeating: 0, count: b.count + 1), count: a.count + 1)
        for i in stride(from: a.count - 1, through: 0, by: -1) {
            for j in stride(from: b.count - 1, through: 0, by: -1) {
                lcs[i][j] = a[i] == b[j] ? lcs[i + 1][j + 1] + 1 : max(lcs[i + 1][j], lcs[i][j + 1])
            }
        }
        var out: [Segment] = []
        func push(_ kind: Kind, _ text: String) {
            if let last = out.last, last.kind == kind {
                out[out.count - 1].text += text
            } else {
                out.append(Segment(kind: kind, text: text))
            }
        }
        var i = 0, j = 0
        var removed = "", added = ""
        func flush() {
            if !removed.isEmpty { push(.removed, removed); removed = "" }
            if !added.isEmpty { push(.added, added); added = "" }
        }
        while i < a.count || j < b.count {
            if i < a.count, j < b.count, a[i] == b[j] {
                flush()
                push(.same, a[i])
                i += 1; j += 1
            } else if j < b.count, i == a.count || lcs[i][j + 1] >= lcs[i + 1][j] {
                added += b[j]; j += 1
            } else {
                removed += a[i]; i += 1
            }
        }
        flush()
        return out
    }
}

/// A change waiting on a person, shown where it happened.
///
/// - `.yellow` (flagged): already applied; Keep accepts it, Revert undoes it.
/// - `.red` (parked): not applied; Apply accepts it, Discard drops it.
///
/// Old words are struck through, new ones underlined, so the change reads
/// without colour; the verdict chip carries the word.
struct InlineChangeCard: View {
    let verdict: Verdict
    let author: String
    var when: String?
    let old: String
    let new: String
    /// the gate's or the agent's note
    var note: String?
    var onAccept: () -> Void = {}
    var onReject: () -> Void = {}

    private var acceptTitle: String { verdict == .red ? "Apply" : "Keep" }
    private var rejectTitle: String { verdict == .red ? "Discard" : "Revert" }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                VerdictChip(verdict)
                AgentBadge(name: author, size: 18)
                Spacer(minLength: 4)
                if let when {
                    Text(when).font(.caption).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                }
            }
            Text(diffText)
                .lapisFont(.docBody)
                .foregroundStyle(Color.lapis.ink)
                .fixedSize(horizontal: false, vertical: true)
                .textSelection(.enabled)
                .accessibilityLabel(spokenDiff)
            if let note, !note.isEmpty {
                Text(note).font(.footnote).foregroundStyle(Color.lapis.ink2)
            }
            HStack(spacing: 8) {
                Button {
                    Haptics.success()
                    onAccept()
                } label: {
                    Label(acceptTitle, systemImage: "checkmark")
                }
                .buttonStyle(.lapis(verdict == .red ? .primary : .keep, size: .small))
                Button {
                    Haptics.impact()
                    onReject()
                } label: {
                    Label(rejectTitle, systemImage: verdict == .red ? "xmark" : "arrow.uturn.backward")
                }
                .buttonStyle(.lapis(verdict == .red ? .destructive : .revert, size: .small))
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(verdict.soft, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .overlay(alignment: .leading) {
            UnevenRoundedRectangle(topLeadingRadius: Lapis.Radius.card, bottomLeadingRadius: Lapis.Radius.card, style: .continuous)
                .fill(verdict.mark)
                .frame(width: 4)
                .accessibilityHidden(true)
        }
        .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.card, style: .continuous).strokeBorder(verdict.mark.opacity(0.35), lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(verdict.spoken) change by \(author)")
    }

    private var segments: [WordDiff.Segment] { WordDiff.segments(old: old, new: new) }

    private var diffText: AttributedString {
        var out = AttributedString()
        for seg in segments {
            var part = AttributedString(seg.text)
            switch seg.kind {
            case .same:
                break
            case .removed:
                part.strikethroughStyle = .single
                part.strikethroughColor = UIColor.lapis.red
                part.foregroundColor = Color.lapis.ink3
            case .added:
                part.underlineStyle = .single
                part.underlineColor = verdict == .red ? UIColor.lapis.red : UIColor.lapis.yellowMark
                part.backgroundColor = verdict == .red ? Color.lapis.redSoft : Color.lapis.mark
            }
            out += part
        }
        return out
    }

    private var spokenDiff: String {
        let removed = segments.filter { $0.kind == .removed }.map(\.text).joined(separator: " … ")
        let added = segments.filter { $0.kind == .added }.map(\.text).joined(separator: " … ")
        return [removed.isEmpty ? nil : "Removed: \(removed)", added.isEmpty ? nil : "Added: \(added)", "Now reads: \(verdict == .red ? old : new)"]
            .compactMap { $0 }.joined(separator: ". ")
    }
}

#Preview("Inline change · light") {
    LapisPreview(title: "Inline change") { changeGallery }.preferredColorScheme(.light)
}

#Preview("Inline change · dark") {
    LapisPreview(title: "Inline change") { changeGallery }.preferredColorScheme(.dark)
}

@MainActor @ViewBuilder private var changeGallery: some View {
    InlineChangeCard(
        verdict: .yellow, author: "claude:docs", when: "2 min ago",
        old: "The sync engine retries every 5 seconds.",
        new: "The sync engine backs off from 1 to 30 seconds, with jitter."
    )
    InlineChangeCard(
        verdict: .red, author: "keeper:API", when: "1 h ago",
        old: "Tokens never expire.", new: "Tokens expire after 30 days.",
        note: "Touches a decision block; parked for a human."
    )
}
