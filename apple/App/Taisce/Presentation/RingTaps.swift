import Foundation
import Observation

/// Tapping a to-do's ring, like Reminders: the row checks at once, fades
/// with a strike-through, and only then is the done toggle queued (the
/// same typed write as swipe-Done). A second tap inside the fade undoes it
/// and nothing is sent.
@MainActor @Observable
final class RingTaps {
    /// how long a checked row lingers before it is committed
    static let fade: Duration = .milliseconds(900)

    /// rows shown checked, waiting out the fade
    private(set) var checked: Set<String> = []
    @ObservationIgnored private var pending: [String: Task<Void, Never>] = [:]
    @ObservationIgnored private let sleep: @Sendable (Duration) async throws -> Void

    init(sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }) {
        self.sleep = sleep
    }

    func isChecked(_ e: TodoEntry) -> Bool { checked.contains(e.id) }

    /// Check (then commit after the fade), or undo a check still fading.
    func tap(_ e: TodoEntry, commit: @escaping @MainActor (TodoEntry) async -> Void) {
        if let task = pending.removeValue(forKey: e.id) {
            task.cancel()
            checked.remove(e.id)
            return
        }
        checked.insert(e.id)
        let sleep = sleep
        pending[e.id] = Task { [weak self] in
            do { try await sleep(Self.fade) } catch { return }
            guard !Task.isCancelled, let self, self.pending[e.id] != nil else { return }
            self.pending[e.id] = nil
            await commit(e)
            // the board hides it now (settled); drop the local check
            self.checked.remove(e.id)
        }
    }

    /// Wait for the fades in flight (tests).
    func settle() async {
        for task in pending.values { await task.value }
    }
}
