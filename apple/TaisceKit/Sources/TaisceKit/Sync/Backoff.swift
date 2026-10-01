import Foundation

/// Exponential reconnect delay with jitter: base · 2^attempt, capped, scaled
/// by a factor in [0.5, 1) so a fleet of clients doesn't reconnect in step.
public struct Backoff: Sendable {
    public var base: Duration
    public var cap: Duration
    let jitter: @Sendable () -> Double

    public init(
        base: Duration = .seconds(1),
        cap: Duration = .seconds(30),
        jitter: @escaping @Sendable () -> Double = { Double.random(in: 0.5..<1) }
    ) {
        self.base = base
        self.cap = cap
        self.jitter = jitter
    }

    public func delay(attempt: Int) -> Duration {
        let exp = base * Double(1 << min(attempt, 16))
        return min(exp, cap) * jitter()
    }
}
