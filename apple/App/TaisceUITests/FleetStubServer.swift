import Foundation
import Network

/// A stand-in server for the Agents UI tests, inside the test runner: plain
/// HTTP/1.1 on a loopback port. It answers like a daemon that needs no
/// sign-in and holds no docs, and serves a fleet the test scripts
/// (`GET /api/fabric/sessions`, the `stream`, `POST` / `DELETE …/snooze`) in
/// the JSON `crates/daemon/src/fabric/routes.rs` sends. Everything else is
/// the daemon's 404. A real fleet needs a signed-in SERVER daemon and a
/// runtime, which a simulator cannot sign in to by itself.
final class FleetStubServer: @unchecked Sendable {
    private let listener: NWListener
    private let lock = NSLock()
    private var fleet: [String: Any]
    private var hasFleet: Bool
    private var streams: [NWConnection] = []
    private var log: [(method: String, path: String, body: [String: Any])] = []
    let port: UInt16

    /// `fleet`: the body of `GET /api/fabric/sessions`; nil = this server has no fleet (404, as LOCAL mode).
    init(fleet: [String: Any]?) throws {
        self.fleet = fleet ?? [:]
        hasFleet = fleet != nil
        let params = NWParameters.tcp
        params.requiredLocalEndpoint = NWEndpoint.hostPort(host: "127.0.0.1", port: .any)
        listener = try NWListener(using: params)
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { state in
            switch state {
            case .ready, .failed, .cancelled: ready.signal()
            default: break
            }
        }
        var bound: UInt16 = 0
        listener.newConnectionHandler = { _ in }
        listener.start(queue: .global())
        _ = ready.wait(timeout: .now() + 5)
        bound = listener.port?.rawValue ?? 0
        guard bound != 0 else { throw URLError(.cannotConnectToHost) }
        port = bound
        listener.newConnectionHandler = { [weak self] conn in
            conn.start(queue: .global())
            self?.read(conn, Data())
        }
    }

    deinit { listener.cancel() }

    var url: String { "http://127.0.0.1:\(port)" }

    /// The requests made of the fabric routes, in order.
    var fabricRequests: [(method: String, path: String, body: [String: Any])] {
        lock.lock(); defer { lock.unlock() }
        return log
    }

    /// Change one session and say so on every open stream, as the hub does.
    func update(session id: String, _ change: (inout [String: Any]) -> Void) {
        lock.lock()
        var sessions = fleet["sessions"] as? [[String: Any]] ?? []
        guard let i = sessions.firstIndex(where: { $0["id"] as? String == id }) else { lock.unlock(); return }
        change(&sessions[i])
        fleet["sessions"] = sessions
        let event = bump("session", sessions[i])
        let open = streams
        lock.unlock()
        open.forEach { $0.send(content: event, completion: .contentProcessed { _ in }) }
    }

    /// Change the (first) runtime and say so on every open stream.
    func updateRuntime(_ change: (inout [String: Any]) -> Void) {
        lock.lock()
        var runtimes = fleet["runtimes"] as? [[String: Any]] ?? []
        guard !runtimes.isEmpty else { lock.unlock(); return }
        change(&runtimes[0])
        fleet["runtimes"] = runtimes
        let event = bump("runtime", runtimes[0])
        let open = streams
        lock.unlock()
        open.forEach { $0.send(content: event, completion: .contentProcessed { _ in }) }
    }

    /// Next rev, and the SSE event carrying it. Call with the lock held.
    private func bump(_ name: String, _ value: [String: Any]) -> Data {
        let rev = (fleet["rev"] as? Int ?? 0) + 1
        fleet["rev"] = rev
        return Data("id: \(rev)\nevent: \(name)\ndata: \(Self.json(value))\n\n".utf8)
    }

    private static func json(_ value: Any) -> String {
        String(decoding: (try? JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])) ?? Data("null".utf8), as: UTF8.self)
    }

    // MARK: HTTP

    private func read(_ conn: NWConnection, _ buffer: Data) {
        conn.receive(minimumIncompleteLength: 1, maximumLength: 65536) { [weak self] data, _, done, error in
            var buffer = buffer
            if let data { buffer.append(data) }
            guard let self else { return conn.cancel() }
            if let head = buffer.range(of: Data("\r\n\r\n".utf8)) {
                let lines = String(decoding: buffer[..<head.lowerBound], as: UTF8.self).components(separatedBy: "\r\n")
                let length = lines.compactMap { line -> Int? in
                    let parts = line.split(separator: ":", maxSplits: 1)
                    guard parts.count == 2, parts[0].lowercased() == "content-length" else { return nil }
                    return Int(parts[1].trimmingCharacters(in: .whitespaces))
                }.first ?? 0
                let body = buffer[head.upperBound...]
                if body.count >= length {
                    let request = lines.first?.split(separator: " ").map(String.init) ?? []
                    return self.answer(conn, method: request.first ?? "GET", target: request.dropFirst().first ?? "/", body: Data(body.prefix(length)))
                }
            }
            if done || error != nil { return conn.cancel() }
            self.read(conn, buffer)
        }
    }

    private func answer(_ conn: NWConnection, method: String, target: String, body: Data) {
        let path = String(target.split(separator: "?").first ?? "")
        let parts = path.split(separator: "/").map(String.init)
        func reply(_ status: String, _ json: String, headers: String = "") {
            let head = "HTTP/1.1 \(status)\r\nContent-Type: application/json\r\nContent-Length: \(json.utf8.count)\r\n\(headers)Connection: close\r\n\r\n"
            conn.send(content: Data((head + json).utf8), completion: .contentProcessed { _ in conn.cancel() })
        }
        let notFound = { reply("404 Not Found", #"{"error":"not found"}"#) }
        if path.hasPrefix("/api/fabric/") {
            lock.lock()
            log.append((method, path, (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] ?? [:]))
            let has = hasFleet
            lock.unlock()
            guard has else { return notFound() }
        }
        switch (method, path) {
        case ("GET", "/api/docs"):
            reply("200 OK", "[]", headers: "Taisce-Seq: 0\r\n")
        case ("GET", "/api/changes"):
            reply("200 OK", #"{"seq":0,"changes":[],"more":false}"#)
        case ("GET", "/api/changes/stream"):
            // an idle journal: held open
            conn.send(content: Data("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\nretry: 3000\n\n".utf8), completion: .contentProcessed { _ in })
        case ("GET", "/api/fabric/sessions"):
            lock.lock(); let body = Self.json(fleet); lock.unlock()
            reply("200 OK", body)
        case ("GET", "/api/fabric/stream"):
            lock.lock()
            let opening = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\n\r\nretry: 3000\n\nid: \(fleet["rev"] as? Int ?? 0)\nevent: fleet\ndata: \(Self.json(fleet))\n\n"
            streams.append(conn)
            lock.unlock()
            conn.send(content: Data(opening.utf8), completion: .contentProcessed { _ in })
        case ("POST", _) where parts.count == 6 && parts.last == "snooze", ("DELETE", _) where parts.count == 6 && parts.last == "snooze":
            let until: Any = method == "POST" ? ((try? JSONSerialization.jsonObject(with: body)) as? [String: Any])?["until"] ?? NSNull() : NSNull()
            lock.lock()
            let known = (fleet["sessions"] as? [[String: Any]] ?? []).contains { $0["id"] as? String == parts[4] && $0["runtime_id"] as? String == parts[3] }
            lock.unlock()
            guard known else { return notFound() }
            update(session: parts[4]) { $0["quiet_until"] = until }
            lock.lock()
            let session = (fleet["sessions"] as? [[String: Any]] ?? []).first { $0["id"] as? String == parts[4] } ?? [:]
            lock.unlock()
            reply("200 OK", Self.json(session))
        default:
            notFound()
        }
    }
}

/// Fleet JSON in the server's shapes (`fabric/view.rs`).
enum StubFleet {
    static let runtime = "01a1fb15-5eea-78d0-a6b7-adbb1135747c"

    static func session(_ n: Int, _ address: String, _ state: String, tool: String = "", note: String = "", last: String = "", account: String = "personal", context: Any = NSNull(), ago: Int = 300) -> [String: Any] {
        let since = Int(Date().timeIntervalSince1970) - ago
        return [
            "id": "0196f3a0-0000-7000-8000-00000000000\(n)", "runtime_id": runtime, "address": address, "fabric": "", "project": "",
            "worktree": "", "branch": "main", "account": account, "state": state, "tool": tool, "note": note, "last_line": last,
            "context_used_pct": context, "since": since, "updated_at": since, "quiet_until": NSNull(),
        ]
    }

    static func id(_ n: Int) -> String { "0196f3a0-0000-7000-8000-00000000000\(n)" }

    static func mac(online: Bool, seenAgo: Int? = 0, accounts: [[String: Any]] = []) -> [String: Any] {
        let now = Int(Date().timeIntervalSince1970)
        return [
            "id": runtime, "name": "mbp", "created_at": now - 86400, "online": online,
            "last_seen_at": seenAgo.map { now - $0 as Any } ?? NSNull(), "version": "stub", "accounts": accounts, "accounts_at": accounts.isEmpty ? NSNull() : now as Any,
        ]
    }

    static var accounts: [[String: Any]] { [
        ["name": "personal", "session_used_pct": 38, "session_resets_at": NSNull(), "week_used_pct": 61, "week_resets_at": NSNull()],
        ["name": "work", "session_used_pct": 92, "session_resets_at": NSNull(), "week_used_pct": 70, "week_resets_at": NSNull()],
    ] }

    /// The server's attention order: needs you, stalled, finished, working; then one at rest.
    static var sessions: [[String: Any]] {
        [
            session(1, "taisce/main", "needs_you", last: "The tests pass. Shall I open the pull request?", context: 44),
            session(2, "qompass/mcp-dx", "stalled", last: "Password:", account: "work", ago: 900),
            session(3, "workbox/main", "finished", last: "Done: the hook receiver checks Host, Origin and the token.", account: "work", context: 19),
            session(4, "taisce/fabric", "working", tool: "Bash", note: "Building the app", context: 61, ago: 7300),
            session(5, "notes/main", "idle"),
        ]
    }

    static func fleet(online: Bool = true, seenAgo: Int? = 0, sessions: [[String: Any]] = StubFleet.sessions) -> [String: Any] {
        ["rev": 9, "runtimes": [mac(online: online, seenAgo: seenAgo, accounts: accounts)], "sessions": sessions]
    }
}
