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

    /// The tree plus `Taisce-Seq` (`X-Grimoire-Seq` on daemons before the
    /// rename), the change-log head read under the same
    /// lock as the list: page `/api/changes` from it to bootstrap without a race.
    /// `seq` is nil on daemons that predate the change log.
    public func treeWithSeq() async throws -> (docs: [DocSummary], seq: Int?) {
        let (data, response) = try await data(for: try await request("/api/docs"))
        try Self.check(data: data, response: response)
        let http = response as? HTTPURLResponse
        let seq = (http?.value(forHTTPHeaderField: "Taisce-Seq") ?? http?.value(forHTTPHeaderField: "X-Grimoire-Seq")).flatMap(Int.init)
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

    /// `workspace` limits hits to docs resolving to it (nil = everywhere).
    public func search(_ query: String, scope: DocID? = nil, workspace: WorkspaceScope? = nil) async throws -> [SearchHit] {
        var q = [URLQueryItem(name: "q", value: query)]
        if let scope { q.append(URLQueryItem(name: "scope", value: scope)) }
        if let workspace { q.append(URLQueryItem(name: "workspace", value: workspace.param)) }
        return try await get("/api/search", query: q)
    }

    /// Open to-dos with deadlines up to `until` (a day, RFC 3339, or a local
    /// `YYYY-MM-DDTHH:MM`; nil = every deadline), soonest alert first,
    /// overdue computed by time. Read-only.
    /// `workspace` reads that workspace's list; nil reads EVERY list (each
    /// item then carries its `docID` and `workspaceID`), for alerts.
    public func todoDue(until: String? = nil, workspace: WorkspaceScope? = nil) async throws -> TodoDueList {
        var q = until.map { [URLQueryItem(name: "until", value: $0)] } ?? []
        if let workspace { q.append(URLQueryItem(name: "workspace", value: workspace.param)) }
        return try await get("/api/todo/due", query: q)
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

    /// A new, empty doc (nil parent = top level). Fill it with `proposeMarkdown`.
    /// `requestID` is sent for servers that dedupe creates on it; callers
    /// retrying after a lost answer should look for the doc first
    /// (`NewDoc.find`), as a server that ignores it would create a second.
    /// `workspaceID` labels a new ROOT doc (a child inherits its parent's).
    public func createDoc(title: String, parent: DocID? = nil, requestID: String? = nil, workspaceID: WorkspaceID? = nil) async throws -> DocSummary {
        struct Body: Encodable { var title: String; var parent_doc_id: DocID?; var request_id: String?; var workspace_id: WorkspaceID? }
        return try await post("/api/docs", body: Body(title: title, parent_doc_id: parent, request_id: requestID, workspace_id: parent == nil ? workspaceID : nil))
    }

    /// Move a doc and everything under it to the Trash (restorable from the
    /// web UI). Answers how many docs went; the `deleted` change rows follow
    /// through sync.
    @discardableResult
    public func deleteDoc(_ id: DocID) async throws -> Int {
        struct Empty: Encodable {}
        struct Deleted: Decodable { var deleted: Int }
        let d: Deleted = try await post("/api/doc/\(id)/delete", body: Empty())
        return d.deleted
    }

    /// Op ids on `docID` waiting for a human (`GET /api/doc/{id}/review`).
    public func openReviewOps(_ docID: DocID) async throws -> Set<String> {
        struct Row: Decodable {
            struct Item: Decodable {
                struct Annotation: Decodable { var op_id: String; var status: String? }
                var annotation: Annotation
            }
            var item: Item
        }
        let rows: [Row] = try await get("/api/doc/\(docID)/review")
        return Set(rows.filter { ($0.item.annotation.status ?? "open") == "open" }.map(\.item.annotation.op_id))
    }

    public func propose(_ req: ProposeRequest) async throws -> ProposeOutcome {
        try await post("/api/propose", body: req)
    }

    public func proposeMarkdown(_ req: ProposeMarkdownRequest) async throws -> ProposeOutcome {
        try await post("/api/propose_markdown", body: req)
    }

    // MARK: to-dos
    // No `GET /api/todo` on purpose: a GET for today carries items forward
    // (a server write). Reads go through `todoDue` and the cached To-do doc.

    // Every to-do call carries the device's clock (`TodoClock`): the server
    // has no idea what day it is for you, and SERVER mode refuses calls
    // without `today`. `utc_offset` lets it read a typed time ("due fri 3pm").

    public func todoAdd(date: String, text: String, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await post("/api/todo", body: TodoBody(date: date, text: text, clock: clock))
    }

    public func todoToggle(date: String, itemID: String, done: Bool, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await post("/api/todo/toggle", body: TodoBody(date: date, itemID: itemID, done: done, clock: clock))
    }

    public func todoEdit(date: String, itemID: String, text: String, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await post("/api/todo/edit", body: TodoBody(date: date, itemID: itemID, text: text, clock: clock))
    }

    public func todoRemove(date: String, itemID: String, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await post("/api/todo/remove", body: TodoBody(date: date, itemID: itemID, clock: clock))
    }

    public func todoMove(date: String, itemID: String, to toDate: String, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await post("/api/todo/move", body: TodoBody(date: date, itemID: itemID, toDate: toDate, clock: clock))
    }

    public func todoSetNote(date: String, itemID: String, note: String, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await post("/api/todo/note", body: TodoBody(date: date, itemID: itemID, note: note, clock: clock))
    }

    /// `nil` clears the deadline; `.allDay` sends `deadline`, `.at` sends
    /// `due_at` (UTC) plus the local day as `deadline`, so a pre-UTC server
    /// still files it on the right day.
    public func todoSetDeadline(date: String, itemID: String, deadline: Deadline?, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await post("/api/todo/deadline", body: DeadlineBody(date: date, itemID: itemID, deadline: deadline, clock: clock))
    }

    /// A wall-clock `Due` picked on this device (date only = all-day).
    public func todoSetDeadline(date: String, itemID: String, deadline: Due?, clock: TodoClock = TodoClock()) async throws -> TodoDay {
        try await todoSetDeadline(date: date, itemID: itemID, deadline: deadline.flatMap { Deadline.local($0) }, clock: clock)
    }

    /// The body of every to-do write: the address, the change, the clock.
    struct TodoBody: Encodable {
        var date: String
        var itemID: String?
        var text: String?
        var done: Bool?
        var toDate: String?
        var note: String?
        var clock: TodoClock

        init(date: String, itemID: String? = nil, text: String? = nil, done: Bool? = nil, toDate: String? = nil, note: String? = nil, clock: TodoClock) {
            self.date = date
            self.itemID = itemID
            self.text = text
            self.done = done
            self.toDate = toDate
            self.note = note
            self.clock = clock
        }

        enum CodingKeys: String, CodingKey {
            case date, text, done, note, today
            case itemID = "item_id"
            case toDate = "to_date"
            case utcOffset = "utc_offset"
            case workspace
        }

        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(date, forKey: .date)
            try c.encodeIfPresent(clock.workspace, forKey: .workspace)
            try c.encodeIfPresent(itemID, forKey: .itemID)
            try c.encodeIfPresent(text, forKey: .text)
            try c.encodeIfPresent(done, forKey: .done)
            try c.encodeIfPresent(toDate, forKey: .toDate)
            try c.encodeIfPresent(note, forKey: .note)
            try c.encode(clock.today, forKey: .today)
            try c.encode(clock.utcOffset, forKey: .utcOffset)
        }
    }

    struct DeadlineBody: Encodable {
        var date: String
        var itemID: String
        var deadline: Deadline?
        var clock: TodoClock

        enum CodingKeys: String, CodingKey {
            case date, deadline, today
            case itemID = "item_id"
            case dueAt = "due_at"
            case workspace
        }

        func encode(to encoder: any Encoder) throws {
            var c = encoder.container(keyedBy: CodingKeys.self)
            try c.encode(date, forKey: .date)
            try c.encodeIfPresent(clock.workspace, forKey: .workspace)
            try c.encode(itemID, forKey: .itemID)
            try c.encode(clock.today, forKey: .today)
            switch deadline {
            case nil:
                // neither clears it
                try c.encodeNil(forKey: .deadline)
            case let .allDay(d)?:
                try c.encode(d, forKey: .deadline)
            case let d?:
                try c.encode(d.wire.dueAt, forKey: .dueAt)
                try c.encode(d.day(in: clock.timeZone), forKey: .deadline)
            }
        }
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
        let msg = errorMessage(in: data)
        if let http = response as? HTTPURLResponse, !(200..<300).contains(http.statusCode) {
            let status = http.statusCode
            if status == 404 { throw APIError.notFound(msg ?? response.url?.path() ?? "") }
            // keep the status for the retryable ones; a 4xx message is the server's answer
            if let msg, !APIError.http(status: status).isTransient { throw APIError.server(msg) }
            throw APIError.http(status: status)
        }
        if let msg {
            throw msg.hasPrefix("not found") ? APIError.notFound(msg) : APIError.server(msg)
        }
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
