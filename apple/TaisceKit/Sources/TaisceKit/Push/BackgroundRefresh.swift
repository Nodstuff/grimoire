import Foundation
import Synchronization

/// What a silent push's background run found, for iOS's
/// `UIBackgroundFetchResult` (which uses it to budget future wake-ups).
public enum BackgroundRefreshResult: Equatable, Sendable {
    case newData
    case noData
    case failed
}

/// Outcome of `withTimeLimit`.
public enum TimeLimited<T: Sendable>: Sendable {
    case finished(T)
    case threw(String)
    case timedOut
}

/// One answer, first come first served, for `withTimeLimit`.
private final class FirstAnswer<T: Sendable>: Sendable {
    private struct State {
        var done = false
        var continuation: CheckedContinuation<TimeLimited<T>, Never>?
        var early: TimeLimited<T>?
    }

    private let state = Mutex(State())

    /// The first answer wins; later ones are dropped.
    func finish(_ value: TimeLimited<T>) {
        let c: CheckedContinuation<TimeLimited<T>, Never>? = state.withLock { s in
            guard !s.done else { return nil }
            s.done = true
            guard let c = s.continuation else {
                s.early = value
                return nil
            }
            s.continuation = nil
            return c
        }
        c?.resume(returning: value)
    }

    func wait() async -> TimeLimited<T> {
        await withCheckedContinuation { (c: CheckedContinuation<TimeLimited<T>, Never>) in
            let early: TimeLimited<T>? = state.withLock { s in
                if let e = s.early { return e }
                s.continuation = c
                return nil
            }
            if let early { c.resume(returning: early) }
        }
    }
}

/// Run `operation`, but return after `limit` whatever it is doing: the
/// operation races a sleep and the loser is cancelled. Returns on time even
/// if the operation ignores cancellation (it then finishes on its own,
/// unobserved), which a task group can't promise: a group waits for every
/// child before it returns.
public func withTimeLimit<T: Sendable>(_ limit: Duration, _ operation: @escaping @Sendable () async throws -> T) async -> TimeLimited<T> {
    let answer = FirstAnswer<T>()
    let work = Task {
        do {
            answer.finish(.finished(try await operation()))
        } catch {
            answer.finish(.threw(String(describing: error)))
        }
    }
    let timer = Task {
        // cancelled only when the work won, and then nothing listens
        try? await Task.sleep(for: limit)
        answer.finish(.timedOut)
    }
    let result = await withTaskCancellationHandler {
        await answer.wait()
    } onCancel: {
        answer.finish(.threw(String(describing: CancellationError())))
    }
    work.cancel()
    timer.cancel()
    return result
}

/// A silent push's background run: one bounded catch-up, then the local
/// due alerts are reconciled against whatever the cache now holds (even
/// after a timeout or an error: pages already applied count), then the
/// answer for iOS. Pure, so tests drive it with fakes and a short limit.
public enum BackgroundRefresh {
    /// iOS gives a content-available push about 30 s; leave room for the reconcile.
    public static let defaultLimit: Duration = .seconds(20)

    /// - Parameters:
    ///   - catchUp: one catch-up; answers whether anything changed
    ///   - reconcile: brings the pending local alerts in step with the cache
    public static func run(
        limit: Duration = defaultLimit,
        catchUp: @escaping @Sendable () async throws -> Bool,
        reconcile: @Sendable () async -> Void
    ) async -> BackgroundRefreshResult {
        let outcome = await withTimeLimit(limit, catchUp)
        await reconcile()
        switch outcome {
        case .finished(true): return .newData
        case .finished(false): return .noData
        case .threw, .timedOut: return .failed
        }
    }
}
