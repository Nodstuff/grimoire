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
    /// For drawing: `segments` with a space where a removal meets an addition.
    static func display(old: String, new: String) -> [Segment] {
        spaced(segments(old: old, new: new))
    }

    /// A removal right before an addition with no space between them reads
    /// as one word ("mostly.An"): a space goes between, for the eye only.
    static func spaced(_ segs: [Segment]) -> [Segment] {
        var out = segs
        for i in out.indices.dropLast() where out[i].kind == .removed && out[i + 1].kind == .added {
            if let l = out[i].text.last, let f = out[i + 1].text.first, !l.isWhitespace, !f.isWhitespace {
                out[i].text += " "
            }
        }
        return out
    }

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
    /// the buttons' words (Keep / Revert, Apply / Discard, Apply delete / Dismiss)
    var acceptTitle: String?
    var rejectTitle: String?
    /// accepting destroys text (a parked delete): the first press asks again
    var confirmAccept = false
    /// false for a viewer: the change shows, the decision isn't theirs
    var showsActions = true
    /// a decision is on its way: the buttons wait
    var busy = false
    /// wide layouts (iPad, Mac): the buttons ride in the header row
    var compact = false

    @State private var confirming = false

    private var accept: String { acceptTitle ?? (verdict == .red ? "Apply" : "Keep") }
    private var reject: String { rejectTitle ?? (verdict == .red ? "Discard" : "Revert") }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 6) {
                chip
                LapisAvatar(name: author, size: 22)
                Text(author)
                    .font(Principal.isAgent(author) ? .lapisMono(.footnote, weight: .medium) : .footnote.weight(.medium))
                    .foregroundStyle(Principal.isAgent(author) ? Color.lapis.agent : Color.lapis.ink)
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .layoutPriority(1)
                if compact, let when {
                    Text("· \(when)").font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                }
                Spacer(minLength: 4)
                if compact, showsActions {
                    actions(height: Lapis.Layout.buttonHeight)
                } else if let when {
                    Text(when).font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                }
            }
            Text(diffText)
                .lapisFont(.docBody)
                .foregroundStyle(Color.lapis.ink)
                .fixedSize(horizontal: false, vertical: true)
                .accessibilityLabel(spokenDiff)
            if let note, !note.isEmpty {
                Text(note).font(.footnote).foregroundStyle(Color.lapis.ink3).fixedSize(horizontal: false, vertical: true)
            }
            if !compact, showsActions {
                actions(height: Lapis.Layout.minTarget)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(verdict.soft, in: shape)
        .overlay(shape.strokeBorder(verdict.mark, lineWidth: 1))
        .accessibilityElement(children: .contain)
        .accessibilityLabel("\(verdict.spoken) change by \(author)")
    }

    private var shape: RoundedRectangle { RoundedRectangle(cornerRadius: 20, style: .continuous) }

    /// The verdict on a card-coloured pill (the soft fill is the card's own).
    private var chip: some View {
        HStack(spacing: 4) {
            Image(systemName: verdict.symbol).imageScale(.small).fontWeight(.semibold)
            Text(verdict.longWord).lineLimit(1)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(verdict.ink)
        .padding(.leading, 7)
        .padding(.trailing, 9)
        .padding(.vertical, 3)
        .background(Color.lapis.card, in: .capsule)
        .fixedSize()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(verdict.spoken)
    }

    @ViewBuilder private func actions(height: CGFloat) -> some View {
        HStack(spacing: 8) {
            Button {
                if confirmAccept, !confirming {
                    Haptics.warning()
                    confirming = true
                    return
                }
                confirming = false
                Haptics.success()
                onAccept()
            } label: {
                Label(confirming ? "Confirm \(accept.lowercased())" : accept, systemImage: verdict == .red ? (confirmAccept ? "trash" : "checkmark") : "checkmark")
            }
            .buttonStyle(CardActionStyle(kind: verdict == .yellow ? .keep : (confirmAccept ? .danger : .apply), height: height, fill: !compact))
            .accessibilityIdentifier("change.accept")
            Button {
                confirming = false
                Haptics.impact()
                onReject()
            } label: {
                Label(reject, systemImage: verdict == .red ? "xmark" : "arrow.uturn.backward")
            }
            .buttonStyle(CardActionStyle(kind: .outline, height: height, fill: !compact))
            .accessibilityIdentifier("change.reject")
        }
        .disabled(busy)
        .task(id: confirming) {
            guard confirming else { return }
            try? await Task.sleep(for: .seconds(4))
            confirming = false
        }
    }

    private var segments: [WordDiff.Segment] { WordDiff.display(old: old, new: new) }

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
                part.foregroundColor = Color.lapis.red
                part.backgroundColor = Color.lapis.redSoft
            case .added:
                part.underlineStyle = .thick
                part.underlineColor = UIColor.lapis.green
                part.backgroundColor = Color.lapis.greenSoft
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

/// A decision button on a change card: a capsule (Keep is solid green with
/// white words; Revert / Discard an outline; a destructive Apply is red
/// words on a quiet fill).
struct CardActionStyle: ButtonStyle {
    enum Kind { case keep, apply, danger, outline }
    let kind: Kind
    var height: CGFloat = Lapis.Layout.minTarget
    /// share the row's width
    var fill = true

    func makeBody(configuration: Configuration) -> some View {
        CardActionBody(configuration: configuration, kind: kind, height: height, fill: fill)
    }
}

private struct CardActionBody: View {
    let configuration: ButtonStyleConfiguration
    let kind: CardActionStyle.Kind
    let height: CGFloat
    let fill: Bool
    @Environment(\.isEnabled) private var isEnabled

    var body: some View {
        configuration.label
            .labelStyle(CardActionLabelStyle())
            .font(height >= Lapis.Layout.minTarget ? .callout.weight(.semibold) : .subheadline.weight(.semibold))
            .foregroundStyle(ink)
            .lineLimit(1)
            .minimumScaleFactor(0.8)
            .padding(.horizontal, 14)
            .frame(maxWidth: fill ? .infinity : nil, minHeight: height)
            .background(background, in: .capsule)
            .overlay { if kind == .outline { Capsule().strokeBorder(Color.lapis.line2, lineWidth: 1) } }
            .opacity(isEnabled ? (configuration.isPressed ? 0.75 : 1) : 0.5)
            .scaleEffect(configuration.isPressed ? 0.98 : 1)
            .frame(minHeight: Lapis.Layout.minTarget)
            .contentShape(.rect)
            .animation(Lapis.Motion.fast, value: configuration.isPressed)
    }

    private var ink: Color {
        switch kind {
        case .keep: .white
        case .apply: Color.lapis.onAccent
        case .danger: Color.lapis.red
        case .outline: Color.lapis.ink
        }
    }

    private var background: Color {
        switch kind {
        case .keep: Color.lapis.greenFill
        case .apply: Color.lapis.accent
        case .danger: Color.lapis.redSoft
        case .outline: Color.lapis.card
        }
    }
}

private struct CardActionLabelStyle: LabelStyle {
    func makeBody(configuration: Configuration) -> some View {
        HStack(spacing: 6) {
            configuration.icon.imageScale(.small).fontWeight(.bold)
            configuration.title
        }
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
