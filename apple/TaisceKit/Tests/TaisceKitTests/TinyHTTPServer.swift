import Foundation
import Network
import Synchronization

/// A real HTTP/1.1 server on 127.0.0.1 (a random port) for what
/// `URLProtocol` mocks can't show, such as whether a client follows a
/// redirect. Each request gets `respond(path, headers)`'s raw response;
/// every request is logged.
final class TinyHTTPServer: Sendable {
    struct Request: Sendable {
        var path: String
        var headers: [String: String]
    }

    final class Log: Sendable {
        let requests = Mutex<[Request]>([])
    }

    private let listener: NWListener
    private let log = Log()
    let port: UInt16

    init(_ respond: @escaping @Sendable (Request) -> String) async throws {
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: .any)
        let l = try NWListener(using: params)
        listener = l
        let log = log
        l.newConnectionHandler = { conn in
            conn.start(queue: .global())
            Self.read(conn, Data()) { head in
                let lines = String(decoding: head, as: UTF8.self).components(separatedBy: "\r\n")
                let path = lines.first?.split(separator: " ").dropFirst().first.map(String.init) ?? "/"
                var headers: [String: String] = [:]
                for line in lines.dropFirst() {
                    guard let colon = line.firstIndex(of: ":") else { continue }
                    headers[line[..<colon].lowercased()] = line[line.index(after: colon)...].trimmingCharacters(in: .whitespaces)
                }
                let r = Request(path: path, headers: headers)
                log.requests.withLock { $0.append(r) }
                conn.send(content: Data(respond(r).utf8), completion: .contentProcessed { _ in conn.cancel() })
            }
        }
        let ready = AsyncStream<UInt16?>.makeStream()
        l.stateUpdateHandler = { state in
            switch state {
            case .ready: ready.continuation.yield(l.port?.rawValue)
            case .failed, .cancelled: ready.continuation.yield(nil)
            default: break
            }
        }
        l.start(queue: .global())
        var it = ready.stream.makeAsyncIterator()
        guard let p = await it.next() ?? nil else { throw URLError(.cannotConnectToHost) }
        port = p
    }

    deinit { listener.cancel() }

    var requests: [Request] { log.requests.withLock { $0 } }
    var url: URL { URL(string: "http://127.0.0.1:\(port)/")! }

    /// Up to the end of the headers, plus whatever body came with them.
    private static func read(_ conn: NWConnection, _ buf: Data, done: @escaping @Sendable (Data) -> Void) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { data, _, isComplete, error in
            var b = buf
            if let data { b.append(data) }
            if b.range(of: Data("\r\n\r\n".utf8)) != nil || isComplete || error != nil {
                done(b)
            } else {
                read(conn, b, done: done)
            }
        }
    }
}
