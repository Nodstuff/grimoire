import Foundation
import Observation
import TaisceKit

/// One doc's "edited" time, observable on its own: a Library row reads it,
/// so a time arriving redraws that row, not the whole tree.
@MainActor @Observable
final class EditedTime {
    var date: Date?

    init(date: Date?) { self.date = date }
}

/// The rows' `EditedTime`s, made when a row first asks, kept in step with
/// `AppModel.editMeta`. Not observable itself: looking one up records no
/// dependency, so the tree's body never depends on the times.
@MainActor
final class EditedTimes {
    private var entries: [DocID: EditedTime] = [:]
    private var dates: [DocID: Date] = [:]

    func entry(_ id: DocID) -> EditedTime {
        if let e = entries[id] { return e }
        let e = EditedTime(date: dates[id])
        entries[id] = e
        return e
    }

    /// The model's metadata changed: entries whose time moved take it (an
    /// assignment only where it differs, so the rest stay quiet).
    func follow(_ meta: [DocID: EditMeta]) {
        dates = meta.mapValues(\.date)
        for (id, e) in entries where e.date != dates[id] { e.date = dates[id] }
    }

    /// how many rows have asked (tests)
    var count: Int { entries.count }
}
