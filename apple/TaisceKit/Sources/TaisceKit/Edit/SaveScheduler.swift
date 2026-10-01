import Foundation

/// Debounced saves per key (a block): each `touch` restarts that key's
/// timer; when it runs out, `save` gets the key. `flush` saves everything
/// waiting at once (a structure change, Done, the app going to the
/// background), so nothing typed waits on a timer that may never fire.
@MainActor
public final class SaveScheduler<Key: Hashable & Sendable> {
    public let delay: Duration
    private let save: @MainActor (Set<Key>) async -> Void
    private var timers: [Key: Task<Void, Never>] = [:]

    public init(delay: Duration = .milliseconds(600), save: @escaping @MainActor (Set<Key>) async -> Void) {
        self.delay = delay
        self.save = save
    }

    public var waiting: Set<Key> { Set(timers.keys) }

    public func touch(_ key: Key) {
        timers[key]?.cancel()
        let delay = delay
        timers[key] = Task { [weak self] in
            do { try await Task.sleep(for: delay) } catch { return }
            await self?.fire(key)
        }
    }

    private func fire(_ key: Key) async {
        guard timers.removeValue(forKey: key) != nil else { return }
        await save([key])
    }

    /// Save every waiting key now (one call).
    public func flush() async {
        let keys = Set(timers.keys)
        for t in timers.values { t.cancel() }
        timers = [:]
        if !keys.isEmpty { await save(keys) }
    }

    /// Forget waiting saves without running them.
    public func cancelAll() {
        for t in timers.values { t.cancel() }
        timers = [:]
    }
}
