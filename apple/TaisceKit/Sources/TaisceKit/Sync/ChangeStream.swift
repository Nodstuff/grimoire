import Foundation

extension APIClient {
    /// No bytes for this long (heartbeats come every 25 s) means the stream
    /// is dead even if the socket hasn't noticed. URLRequest's timeout is an
    /// idle timeout, which is exactly that watchdog.
    static let streamIdleTimeout: TimeInterval = 60

    /// Opens `GET /api/changes/stream`, resuming after `lastEventID`, with
    /// the provider's current bearer (each reconnect asks again, so a
    /// refreshed token is picked up; a 401 renews and retries once). The
    /// stream finishes when the server closes and throws on network errors;
    /// reconnecting is the caller's job (see `SyncEngine`).
    public func changeStream(lastEventID: Int?) -> AsyncThrowingStream<SSEOutput, any Error> {
        AsyncThrowingStream { continuation in
            let task = Task {
                do {
                    var req = try await request("/api/changes/stream")
                    req.setValue("text/event-stream", forHTTPHeaderField: "Accept")
                    req.setValue("no-cache", forHTTPHeaderField: "Cache-Control")
                    if let lastEventID {
                        req.setValue(String(lastEventID), forHTTPHeaderField: "Last-Event-ID")
                    }
                    req.timeoutInterval = Self.streamIdleTimeout
                    var (bytes, response) = try await session.bytes(for: req)
                    if Self.isUnauthorized(response), let retry = try await renewed(req) {
                        (bytes, response) = try await session.bytes(for: retry)
                    }
                    if Self.isUnauthorized(response) { throw APIError.unauthorized }
                    if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
                        throw APIError.http(status: http.statusCode)
                    }
                    if response.mimeType == "text/html" { throw APIError.notAPIRoute("/api/changes/stream") }
                    var parser = SSEParser(lastEventID: lastEventID.map(String.init))
                    var out: [SSEOutput] = []
                    for try await b in bytes {
                        parser.feed(byte: b, into: &out)
                        if !out.isEmpty {
                            for o in out { continuation.yield(o) }
                            out.removeAll(keepingCapacity: true)
                        }
                    }
                    continuation.finish()
                } catch {
                    continuation.finish(throwing: error)
                }
            }
            continuation.onTermination = { _ in task.cancel() }
        }
    }
}
