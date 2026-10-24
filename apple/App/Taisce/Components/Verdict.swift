import SwiftUI
import TaisceKit

/// The gate's three verdicts in Lapis: colour, shape and word, never
/// colour alone. Applied is a circled check (green), flagged a flag
/// (yellow: applied, a human should look), parked a paused square (red:
/// not applied until a human accepts).
extension Verdict {
    /// The server's colours and the words people use both map here.
    init?(lapis word: String?) {
        switch word?.lowercased() {
        case "green", "applied": self = .green
        case "yellow", "flagged": self = .yellow
        case "red", "parked": self = .red
        default: return nil
        }
    }

    var word: String {
        switch self {
        case .green: "Applied"
        case .yellow: "Flagged"
        case .red: "Parked"
        }
    }

    /// The word with what it means for the text.
    var longWord: String {
        switch self {
        case .green: "Applied"
        case .yellow: "Flagged · applied"
        case .red: "Parked · not applied"
        }
    }

    var spoken: String {
        switch self {
        case .green: "Applied"
        case .yellow: "Flagged, applied"
        case .red: "Parked, not applied"
        }
    }

    var symbol: String {
        switch self {
        case .green: "checkmark.circle"
        case .yellow: "flag"
        case .red: "pause.rectangle"
        }
    }

    var ink: Color {
        switch self {
        case .green: Color.lapis.green
        case .yellow: Color.lapis.yellow
        case .red: Color.lapis.red
        }
    }

    var soft: Color {
        switch self {
        case .green: Color.lapis.greenSoft
        case .yellow: Color.lapis.yellowSoft
        case .red: Color.lapis.redSoft
        }
    }

    /// A bare mark (a bar, a dot, an icon on a card)
    var mark: Color {
        switch self {
        case .green: Color.lapis.green
        case .yellow: Color.lapis.yellowMark
        case .red: Color.lapis.red
        }
    }
}

/// `VerdictChip(.yellow)` → ⚑ Flagged · applied. `short` drops the tail
/// where space is tight.
struct VerdictChip: View {
    let verdict: Verdict
    var short = false

    init(_ verdict: Verdict, short: Bool = false) {
        self.verdict = verdict
        self.short = short
    }

    var body: some View {
        HStack(spacing: 5) {
            Image(systemName: verdict.symbol)
                .imageScale(.small)
                .fontWeight(.semibold)
            Text(short ? verdict.word : verdict.longWord)
                .lineLimit(1)
        }
        .font(.caption.weight(.semibold))
        .foregroundStyle(verdict.ink)
        .padding(.leading, 7)
        .padding(.trailing, 9)
        .padding(.vertical, 3)
        .background(verdict.soft, in: .capsule)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(verdict.spoken)
    }
}

/// An editor's save state: a mark and a word (Lapis-System "Save state").
/// Marks differ in shape as well as colour: a dot, a ring, a square.
enum SaveState: Hashable, Sendable, CaseIterable {
    case saved, saving, unsaved, failed, waiting, readonly

    var word: String {
        switch self {
        case .saved: "Saved"
        case .saving: "Saving…"
        case .unsaved: "Unsaved"
        case .failed: "Not saved"
        case .waiting: "Waiting for review"
        case .readonly: "Read only"
        }
    }
}

struct SaveStateDot: View {
    let state: SaveState
    /// replaces the word ("Not saved · retrying in 5 s")
    var text: String?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var pulse = false

    var body: some View {
        HStack(spacing: 6) {
            mark
                .frame(width: 8, height: 8)
                .accessibilityHidden(true)
            Text(text ?? state.word)
                .font(.caption.weight(.medium))
                .foregroundStyle(Color.lapis.ink2)
                .lineLimit(1)
        }
        .accessibilityElement(children: .combine)
    }

    @ViewBuilder private var mark: some View {
        switch state {
        case .saved: Circle().fill(Color.lapis.green)
        case .saving:
            Circle().fill(Color.lapis.accent)
                .opacity(pulse ? 0.35 : 1)
                .onAppear {
                    guard !reduceMotion else { return }
                    withAnimation(.easeInOut(duration: 0.8).repeatForever()) { pulse = true }
                }
        case .unsaved: Circle().strokeBorder(Color.lapis.ink3, lineWidth: 1.5)
        case .failed: RoundedRectangle(cornerRadius: 2).fill(Color.lapis.red)
        case .waiting: Circle().fill(Color.lapis.yellowMark)
        case .readonly: RoundedRectangle(cornerRadius: 2).fill(Color.lapis.ink3)
        }
    }
}

#Preview("Verdicts · light") {
    LapisPreview(title: "Verdicts") {
        HStack { VerdictChip(.green); VerdictChip(.yellow); VerdictChip(.red) }
        HStack { VerdictChip(.green, short: true); VerdictChip(.yellow, short: true); VerdictChip(.red, short: true) }
        SectionLabel("Save state")
        VStack(alignment: .leading, spacing: 10) {
            ForEach(SaveState.allCases, id: \.self) { SaveStateDot(state: $0) }
        }
    }
    .preferredColorScheme(.light)
}

#Preview("Verdicts · dark") {
    LapisPreview(title: "Verdicts") {
        HStack { VerdictChip(.green); VerdictChip(.yellow); VerdictChip(.red) }
        SectionLabel("Save state")
        VStack(alignment: .leading, spacing: 10) {
            ForEach(SaveState.allCases, id: \.self) { SaveStateDot(state: $0) }
        }
    }
    .preferredColorScheme(.dark)
}
