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
        let (data, response) = try await data(for: try await request("/api/docs"))
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

    /// Open to-dos with deadlines up to `until` (a day, RFC 3339, or a local
    /// `YYYY-MM-DDTHH:MM`; nil = every deadline), soonest alert first,
    /// overdue computed by time. Read-only.
    public func todoDue(until: String? = nil) async throws -> TodoDueList {
        try await get("/api/todo/due", query: until.map { [URLQueryItem(name: "until", value: $0)] } ?? [])
    }

    /// The change-log head alone (`limit=0`), e.g. to start a cursor without a backfill.
    public func changesHead() async throws -> Int {
        try await changes(since: 0, limit: 0).seq
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
    // No `GET /api/todo` on purpose: a GET for today carries items forward
    // (a server write). Reads go through `todoDue` and the cached To-do doc.

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

    /// `nil` clears the deadline. A date-only `Due` keeps the item's existing
    /// time (server rule); a timed one sets `due_time`.
    public func todoSetDeadline(date: String, itemID: String, deadline: Due?) async throws -> TodoDay {
        try await post("/api/todo/deadline", body: DeadlineBody(date: date, itemID: itemID, deadline: deadline))
    }

    struct DeadlineBody: Encodable {
        var date: String
        var itemID: String
        var deadline: Due?

        enum CodingKeys: String, CodingKey {
            case date, deadline
            case itemID = "item_id"
            case dueTime = "due_time"
        }

        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(date, forKey: .date)
            try c.encode(itemID, forKey: .itemID)
            try c.encode(deadline?.dateString, forKey: .deadline)
            if let d = deadline, let h = d.hour, let m = d.minute {
                try c.encode(String(format: "%02d:%02d", h, m), forKey: .dueTime)
            }
        }
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
        let (data, response) = try await data(for: request)
        try Self.check(data: data, response: response)
        return data
    }

    /// One round trip; a 401 to a bearer request renews the token (single
    /// flight, in the provider) and retries once.
    func data(for request: URLRequest) async throws -> (Data, URLResponse) {
        let (data, response) = try await session.data(for: request)
        guard Self.isUnauthorized(response), let retry = try await renewed(request) else { return (data, response) }
        return try await session.data(for: retry)
    }

    /// `request` with a fresh bearer after a 401, or nil when there is
    /// nothing to renew (no token was sent, or the provider gives up).
    func renewed(_ request: URLRequest) async throws -> URLRequest? {
        guard let auth = request.value(forHTTPHeaderField: "Authorization"), auth.hasPrefix("Bearer "),
              let token = try await config.tokenProvider.renew(rejected: String(auth.dropFirst(7)))
        else { return nil }
        var r = request
        r.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        return r
    }

    static func isUnauthorized(_ response: URLResponse) -> Bool {
        (response as? HTTPURLResponse)?.statusCode == 401
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
        if isUnauthorized(response) { throw APIError.unauthorized }
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
