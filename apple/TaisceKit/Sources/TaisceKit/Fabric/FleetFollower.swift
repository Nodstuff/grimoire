import Foundation

/// The state of the connection a `FleetFollower` keeps.
public enum FleetLink: Sendable, Hashable {
    case idle
    case connecting
    /// the stream is open and has sent its fleet
    case live
    case waiting(retryIn: Duration)
    /// the server has no fleet for us (404: a LOCAL daemon, or a server
    /// without Agent Fabric). Following has stopped.
    case unavailable
}

/// What a follower reports, in order.
public enum FleetSignal: Sendable {
    /// a change, and the fleet's `rev` after it
    case change(FleetChange, rev: Int)
    /// one of the Slice 2–5 events (a turn, an attention item, an order, …)
    case event(FabricStreamEvent)
    case link(FleetLink)
    /// why the last connection ended (the follower is about to wait and retry)
    case failed(any Error)
}

/// Follows `GET /api/fabric/stream`: each connection opens with the whole
/// fleet and then sends one event per change. On a drop it backs off and
/// reconnects the way `SyncEngine` does (the server's `retry:` is the
/// floor; a connection that delivered anything starts the backoff over).
/// There is no cursor: a new connection's fleet replaces what was held. A
/// 404 means there is no fleet here and ends the following. So does a 401
/// that renewing the token did not cure: the session is over, and whoever
/// follows starts again after the next sign-in.
public struct FleetFollower: Sendable {
    let service: any FleetService
    let backoff: Backoff
    let sleep: @Sendable (Duration) async throws -> Void

    public init(
        service: any FleetService,
        backoff: Backoff = Backoff(),
        sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) {
        self.service = service
        self.backoff = backoff
        self.sleep = sleep
    }

    /// Follow until the consumer stops listening (or the server says there
    /// is no fleet). Cancelling the consuming task ends the connection.
    public func signals() -> AsyncStream<FleetSignal> {
        AsyncStream(bufferingPolicy: .unbounded) { continuation in
            let task = Task {
                await run { continuation.yield($0) }
                continuation.finish()
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }

    func run(_ emit: @Sendable (FleetSignal) -> Void) async {
        var attempt = 0
        var serverRetry: Duration?
        while !Task.isCancelled {
            emit(.link(.connecting))
            var healthy = false
            do {
                for try await output in service.fleetStream() {
                    switch output {
                    case .retry(let ms):
                        serverRetry = .milliseconds(ms)
                    case .comment:
                        break
                    case .event(let event):
                        if FabricStreamEvent.names.contains(event.event) {
                            // one fabric event this app cannot read is skipped; the stream goes on
                            if let fabric = try? FabricStreamEvent.decode(event) {
                                healthy = true
                                emit(.event(fabric))
                            }
                            continue
                        }
                        guard let (change, rev) = try FleetChange.decode(event) else { continue }
                        // only what could be read counts: a stream that opens with
                        // something unreadable backs off, it does not spin
                        healthy = true
                        emit(.change(change, rev: rev))
                        if case .fleet = change { emit(.link(.live)) }
                    }
                }
                // the server ended it (it fell behind, or the daemon is restarting): start over
            } catch is CancellationError {
                break
            } catch APIError.http(status: 404), APIError.notAPIRoute, APIError.notFound {
                emit(.link(.unavailable))
                return
            } catch APIError.unauthorized {
                // the token was renewed once and refused again: not ours to retry
                emit(.failed(APIError.unauthorized))
                break
            } catch {
                if Task.isCancelled { break }
                emit(.failed(error))
            }
            if Task.isCancelled { break }
            if healthy { attempt = 0 }
            let delay = serverRetry.map { max($0, backoff.delay(attempt: attempt)) } ?? backoff.delay(attempt: attempt)
            emit(.link(.waiting(retryIn: delay)))
            attempt += 1
            do { try await sleep(delay) } catch { break }
        }
        emit(.link(.idle))
    }
}
