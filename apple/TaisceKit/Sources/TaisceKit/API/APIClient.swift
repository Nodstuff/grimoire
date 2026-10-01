import Foundation

/// Typed access to the daemon's HTTP API (crates/daemon/src/api.rs, todo.rs).
/// Stateless and `Sendable`: share one per server.
public struct APIClient: Sendable {
    public let config: ServerConfig
    let session: URLSession

    public init(config: ServerConfig, session: URLSession = .shared) {
        self.config = config
        self.session = session
    }

    // MARK: read side

    public func tree() async throws -> [DocSummary] {
        try await treeWithSeq().docs
    }

    /// The tree plus `X-Grimoire-Seq`, the change-log head read under the same
    /// lock as the list: page `/api/changes` from it to bootstrap without a race.
    /// `seq` is nil on daemons that predate the change log.
    public func treeWithSeq() async throws -> (docs: [DocSummary], seq: Int?) {
        let (data, response) = try await session.data(for: try await request("/api/docs"))
        try Self.check(data: data, response: response)
        let seq = (response as? HTTPURLResponse)?.value(forHTTPHeaderField: "X-Grimoire-Seq").flatMap(Int.init)
        do {
            return (try JSONDecoder().decode([DocSummary].self, from: data), seq)
        } catch {
            throw APIError.decoding(String(describing: error))
        }
    }

    public func doc(_ id: DocID) async throws -> DocTree {
        try await get("/api/doc/\(id)")
    }

    public func docMarkdown(_ id: DocID) async throws -> String {
        struct Body: Decodable { var markdown: String }
        let b: Body = try await get("/api/doc/\(id)/markdown")
        return b.markdown
    }

    public func search(_ query: String, scope: DocID? = nil) async throws -> [SearchHit] {
        var q = [URLQueryItem(name: "q", value: query)]
        if let scope { q.append(URLQueryItem(name: "scope", value: scope)) }
        return try await get("/api/search", query: q)
    }

    /// NB: a GET for today (or later) may carry-forward yesterday's open items,
    /// i.e. it can write on the server. That is the web UI's behaviour too.
    public func todoDay(_ date: String? = nil) async throws -> TodoDay {
        try await get("/api/todo", query: date.map { [URLQueryItem(name: "date", value: $0)] } ?? [])
    }

    public func changes(since: Int, limit: Int = 500) async throws -> ChangePage {
        try await get("/api/changes", query: [
            URLQueryItem(name: "since", value: String(since)),
            URLQueryItem(name: "limit", value: String(limit)),
        ])
    }

    // MARK: block edits (no UI yet)

    public func propose(_ req: ProposeRequest) async throws -> ProposeOutcome {
        try await post("/api/propose", body: req)
    }

    public func proposeMarkdown(_ req: ProposeMarkdownRequest) async throws -> ProposeOutcome {
        try await post("/api/propose_markdown", body: req)
    }

    // MARK: to-dos

    public func todoAdd(date: String, text: String) async throws -> TodoDay {
        try await post("/api/todo", body: ["date": date, "text": text])
    }

    public func todoToggle(date: String, itemID: String, done: Bool) async throws -> TodoDay {
        struct Body: Encodable { var date: String; var item_id: String; var done: Bool }
        return try await post("/api/todo/toggle", body: Body(date: date, item_id: itemID, done: done))
    }

    public func todoEdit(date: String, itemID: String, text: String) async throws -> TodoDay {
        try await post("/api/todo/edit", body: ["date": date, "item_id": itemID, "text": text])
    }

    public func todoRemove(date: String, itemID: String) async throws -> TodoDay {
        try await post("/api/todo/remove", body: ["date": date, "item_id": itemID])
    }

    public func todoMove(date: String, itemID: String, to toDate: String) async throws -> TodoDay {
        try await post("/api/todo/move", body: ["date": date, "item_id": itemID, "to_date": toDate])
    }

    /// `nil` clears the deadline.
    public func todoSetDeadline(date: String, itemID: String, deadline: Due?) async throws -> TodoDay {
        struct Body: Encodable { var date: String; var item_id: String; var deadline: String? }
        return try await post("/api/todo/deadline", body: Body(date: date, item_id: itemID, deadline: deadline?.description))
    }

    public func todoSetNote(date: String, itemID: String, note: String) async throws -> TodoDay {
        try await post("/api/todo/note", body: ["date": date, "item_id": itemID, "note": note])
    }

    // MARK: plumbing

    func url(_ path: String, query: [URLQueryItem] = []) throws -> URL {
        guard var c = URLComponents(url: config.baseURL, resolvingAgainstBaseURL: false) else {
            throw APIError.badURL(config.baseURL.absoluteString)
        }
        c.path = (c.path.hasSuffix("/") ? String(c.path.dropLast()) : c.path) + path
        c.queryItems = query.isEmpty ? nil : query
        guard let u = c.url else { throw APIError.badURL(path) }
        return u
    }

    func request(_ path: String, query: [URLQueryItem] = [], method: String = "GET") async throws -> URLRequest {
        var r = URLRequest(url: try url(path, query: query))
        r.httpMethod = method
        r.setValue("application/json", forHTTPHeaderField: "Accept")
        if let token = try await config.tokenProvider.token() {
            r.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        }
        return r
    }

    func get<T: Decodable>(_ path: String, query: [URLQueryItem] = []) async throws -> T {
        try await send(try await request(path, query: query))
    }

    func post<T: Decodable>(_ path: String, body: some Encodable) async throws -> T {
        var r = try await request(path, method: "POST")
        r.setValue("application/json", forHTTPHeaderField: "Content-Type")
        r.httpBody = try JSONEncoder().encode(body)
        return try await send(r)
    }

    /// Raw send for callers that replay stored requests (the outbox).
    public func send(raw request: URLRequest) async throws -> Data {
        let (data, response) = try await session.data(for: request)
        try Self.check(data: data, response: response)
        return data
    }

    func send<T: Decodable>(_ request: URLRequest) async throws -> T {
        let data = try await send(raw: request)
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw APIError.decoding(String(describing: error))
        }
    }

    static func check(data: Data, response: URLResponse) throws {
        if response.mimeType == "text/html" {
            throw APIError.notAPIRoute(response.url?.path() ?? "")
        }
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            if let msg = errorMessage(in: data) { throw APIError.server(msg) }
            throw APIError.http(status: http.statusCode)
        }
        if let msg = errorMessage(in: data) { throw APIError.server(msg) }
    }

    /// The `{"error": "..."}` envelope, if `data` is one.
    static func errorMessage(in data: Data) -> String? {
        guard data.first == UInt8(ascii: "{"),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let msg = obj["error"] as? String
        else { return nil }
        return msg
    }
}
