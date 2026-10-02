import Foundation
import TaisceKit

/// The edit-mode sync chip: what happened to what you typed.
struct EditorChip: Hashable, Sendable {
    enum Tone: Hashable, Sendable { case saved, busy, offline, review, conflict, failed }

    var tone: Tone
    var text: String

    /// Worst news first: refused writes, a live session holding the queue,
    /// parked (red) edits, then the queue itself, then flagged (yellow) ones.
    static func make(online: Bool, unsaved: Int, outbox: DocOutboxState, persistFailures: Int = 0, reviews: [Verdict]) -> EditorChip {
        if persistFailures > 0 {
            return EditorChip(tone: .failed, text: "Couldn't save · Retry")
        }
        if outbox.failed > 0 {
            return EditorChip(tone: .failed, text: outbox.failed == 1 ? "1 edit not saved" : "\(outbox.failed) edits not saved")
        }
        if outbox.liveSession {
            return EditorChip(tone: .offline, text: "Doc is open in a live session · will retry")
        }
        if reviews.contains(.red) {
            return EditorChip(tone: .conflict, text: "Waiting for review")
        }
        if outbox.pending > 0, !online {
            return EditorChip(tone: .offline, text: "Offline · \(outbox.pending) pending")
        }
        if unsaved > 0 || outbox.pending > 0 {
            return EditorChip(tone: .busy, text: "Saving…")
        }
        if reviews.contains(.yellow) {
            return EditorChip(tone: .review, text: "Waiting for review")
        }
        return EditorChip(tone: .saved, text: "Saved")
    }
}
