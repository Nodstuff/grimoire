import Foundation

/// ClickHouse over its HTTP interface. One POST per statement with
/// `readonly=1` unless the source allows writes: the server refuses
/// anything but reads, and no setting (`readonly` included) can be changed
/// from the query. 1, not 2: `readonly=2` still runs table functions
/// (`url()`, `s3()`, `remote()`, `file()`, …), which reach other servers
/// and files with the server's own access; the cost is that a query's own
/// `SETTINGS …` clause is refused too. Statements of one run share a
/// `session_id`, so `SET` and temporary tables carry from one to the next
/// (with Allow writes; read-only refuses both). Results as `JSONCompactEachRowWithNamesAndTypes`. Stop cancels the
/// request and sends `KILL QUERY` for its `query_id`, best effort.
public struct ClickHouseDriver: SQLDriver {
    public static let format = "JSONCompactEachRowWithNamesAndTypes"

    public var url: URL
    public var database: String
    public var user: String
    public var password: String?
    public var allowWrites: Bool
    let session: URLSession

    public init(url: URL, database: String, user: String, password: String?, allowWrites: Bool, session: URLSession = ClickHouseDriver.defaultSession) {
        self.url = url
        self.database = database
        self.user = user
        self.password = password
        self.allowWrites = allowWrites
        self.session = session
    }

    public init(_ source: DataSource, password: String?, session: URLSession = ClickHouseDriver.defaultSession) throws {
        guard let u = URL(string: source.url.trimmingCharacters(in: .whitespaces)), u.host() != nil else {
            throw SQLDriverError("\(source.name): not a URL: \(source.url)")
        }
        if let p = source.transportProblem { throw SQLDriverError("\(source.name): \(p)") }
        self.init(url: u, database: source.database, user: source.user, password: password, allowWrites: source.allowWrites, session: session)
    }

    /// Never follows a redirect: the password header (`X-ClickHouse-Key`)
    /// would go along to wherever it points. A 3xx is then the answer, and
    /// the run says so.
    public static let defaultSession: URLSession = {
        let c = URLSessionConfiguration.ephemeral
        c.timeoutIntervalForRequest = 310
        c.urlCache = nil
        c.httpCookieStorage = nil
        return URLSession(configuration: c, delegate: NoRedirects(), delegateQueue: nil)
    }()

    final class NoRedirects: NSObject, URLSessionTaskDelegate, Sendable {
        func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest) async -> URLRequest? {
            nil
        }
    }

    // MARK: requests (pure)

    /// The POST for one statement. Settings go in the URL; credentials in
    /// headers, never the URL.
    public func request(_ statement: String, queryID: String, session: String? = nil, readOnly: Bool? = nil) -> URLRequest {
        var c = URLComponents(url: url, resolvingAgainstBaseURL: false) ?? URLComponents()
        var items = c.queryItems ?? []
        // readonly first: settings after it are still applied by the server
        if readOnly ?? !allowWrites { items.append(URLQueryItem(name: "readonly", value: "1")) }
        items.append(URLQueryItem(name: "query_id", value: queryID))
        if let session { items.append(URLQueryItem(name: "session_id", value: session)) }
        items.append(URLQueryItem(name: "default_format", value: Self.format))
        if !database.isEmpty { items.append(URLQueryItem(name: "database", value: database)) }
        c.queryItems = items
        if c.path.isEmpty { c.path = "/" }
        var r = URLRequest(url: c.url ?? url)
        r.httpMethod = "POST"
        r.httpBody = Data(statement.utf8)
        r.setValue("text/plain; charset=utf-8", forHTTPHeaderField: "Content-Type")
        if !user.isEmpty { r.setValue(user, forHTTPHeaderField: "X-ClickHouse-User") }
        if let password, !password.isEmpty { r.setValue(password, forHTTPHeaderField: "X-ClickHouse-Key") }
        return r
    }

    /// `KILL QUERY` for a stopped statement (never read-only: KILL isn't a
    /// read; outside the run's session, which the stopped query holds).
    public func killRequest(queryID: String) -> URLRequest {
        let escaped = queryID.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "'", with: "\\'")
        return request("KILL QUERY WHERE query_id = '\(escaped)' ASYNC", queryID: UUID().uuidString, readOnly: false)
    }

    // MARK: running

    public func run(_ sql: String, cap: Int) async throws -> [SQLStatementResult] {
        var results: [SQLStatementResult] = []
        let session = UUID().uuidString
        for statement in SQLStatements.split(sql, dialect: .clickhouse) {
            try Task.checkCancellation()
            let outcome = try await runOne(statement, cap: cap, session: session)
            results.append(SQLStatementResult(sql: statement, outcome: outcome))
            if case .failed = outcome { break }
        }
        return results
    }

    func runOne(_ statement: String, cap: Int, session sessionID: String? = nil) async throws -> SQLStatementResult.Outcome {
        let id = UUID().uuidString
        let session = session
        let kill = killRequest(queryID: id)
        return try await withTaskCancellationHandler {
            let (bytes, response) = try await session.bytes(for: request(statement, queryID: id, session: sessionID))
            guard let http = response as? HTTPURLResponse else { throw SQLDriverError("Not an HTTP response from \(url.host() ?? "the server")") }
            var parser = ClickHouseResultParser(cap: cap, format: http.value(forHTTPHeaderField: "X-ClickHouse-Format"))
            if http.statusCode != 200 {
                var body = Data()
                for try await b in bytes {
                    body.append(b)
                    if body.count > 64 * 1024 { break }
                }
                let text = String(decoding: body, as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines)
                if (300..<400).contains(http.statusCode) {
                    let to = http.value(forHTTPHeaderField: "Location").map { " to \($0)" } ?? ""
                    throw SQLDriverError("ClickHouse answered with a redirect (HTTP \(http.statusCode))\(to); not followed, so the password stays put. Use the URL it points to.")
                }
                if http.statusCode == 401 || http.statusCode == 403 || text.contains("(AUTHENTICATION_FAILED)") || text.contains("(REQUIRED_PASSWORD)") {
                    throw SQLDriverError(text.isEmpty ? "ClickHouse refused the user or password (HTTP \(http.statusCode))" : text)
                }
                return .failed(text.isEmpty ? "HTTP \(http.statusCode)" : text)
            }
            for try await line in bytes.lines {
                parser.add(line)
            }
            return parser.outcome(summary: http.value(forHTTPHeaderField: "X-ClickHouse-Summary"))
        } onCancel: {
            Task.detached { _ = try? await session.data(for: kill) }
        }
    }

    public func testConnection() async throws -> String {
        let r = try await run("SELECT version()", cap: 1)
        if let e = r.last?.error { throw SQLDriverError(e) }
        guard case .rows(let set) = r.last?.outcome, let v = set.rows.first?.first ?? nil else {
            throw SQLDriverError("ClickHouse answered, but not with a version")
        }
        return "ClickHouse \(v)\(allowWrites ? "" : ", read-only")"
    }
}

/// `JSONCompactEachRowWithNamesAndTypes`, a line at a time: names, types,
/// then one JSON array per row. A line that isn't an array (an exception
/// the server appends mid-stream) ends it as an error. Another format (a
/// `FORMAT …` in the query) comes back as one text column. Pure.
public struct ClickHouseResultParser: Sendable {
    var collector: SQLRowCollector
    var names: [String]?
    var types: [String]?
    /// the response isn't ours to parse: its lines are the rows
    let rawFormat: String?
    var errorLines: [String] = []
    public private(set) var failed: String?

    public init(cap: Int, format: String?) {
        collector = SQLRowCollector(cap: cap)
        rawFormat = (format == nil || format == ClickHouseDriver.format) ? nil : format
    }

    public mutating func add(_ line: String) {
        if let rawFormat {
            if names == nil { names = ["output (\(rawFormat))"]; types = [] }
            collector.add([line])
            return
        }
        if !errorLines.isEmpty {
            if errorLines.count < 50 { errorLines.append(line) }
            return
        }
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        if trimmed.isEmpty { return }
        if names == nil {
            guard let n = Self.elements(trimmed) else { return fail(line) }
            names = n.map { $0 ?? "" }
        } else if types == nil {
            guard let t = Self.elements(trimmed) else { return fail(line) }
            types = t.map { $0 ?? "" }
        } else if collector.isFull, trimmed.hasPrefix("[") {
            collector.count()
        } else if let row = Self.elements(trimmed) {
            collector.add(row)
        } else {
            fail(line)
        }
    }

    mutating func fail(_ line: String) {
        errorLines.append(line)
        failed = line
    }

    public func outcome(summary: String?) -> SQLStatementResult.Outcome {
        if !errorLines.isEmpty {
            let text = errorLines.filter { !$0.hasPrefix("__exception__") }.joined(separator: "\n").trimmingCharacters(in: .whitespacesAndNewlines)
            return .failed(text.isEmpty ? "ClickHouse broke off the result" : text)
        }
        guard let names else { return .done(rowsAffected: Self.writtenRows(summary)) }
        let cols = names.enumerated().map { i, n in SQLColumn(n, type: types.flatMap { i < $0.count ? $0[i] : nil }) }
        return .rows(collector.result(cols))
    }

    /// `written_rows` from `X-ClickHouse-Summary` (nil when 0 or absent).
    static func writtenRows(_ summary: String?) -> Int? {
        guard let data = summary?.data(using: .utf8),
              let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        let v = obj["written_rows"]
        let n = (v as? String).flatMap(Int.init) ?? (v as? Int)
        return n == 0 ? nil : n
    }

    /// A JSON array's elements as display text: strings unquoted, numbers
    /// and booleans as written, `null` as nil, arrays and objects as their
    /// JSON. nil when `line` isn't an array.
    public static func elements(_ line: String) -> [String?]? {
        let s = Array(line.utf8)
        guard s.first == UInt8(ascii: "["), s.last == UInt8(ascii: "]") else { return nil }
        var out: [String?] = []
        var i = 1
        let end = s.count - 1
        func skipSpace() { while i < end, s[i] == 0x20 || s[i] == 0x09 || s[i] == 0x0A || s[i] == 0x0D { i += 1 } }
        skipSpace()
        if i == end { return [] }
        while i < end {
            skipSpace()
            let start = i
            var depth = 0
            var inString = false
            while i < end {
                let c = s[i]
                if inString {
                    if c == UInt8(ascii: "\\") { i += 2; continue }
                    if c == UInt8(ascii: "\"") { inString = false }
                } else if c == UInt8(ascii: "\"") {
                    inString = true
                } else if c == UInt8(ascii: "[") || c == UInt8(ascii: "{") {
                    depth += 1
                } else if c == UInt8(ascii: "]") || c == UInt8(ascii: "}") {
                    depth -= 1
                } else if c == UInt8(ascii: ","), depth == 0 {
                    break
                }
                i += 1
            }
            guard !inString, depth == 0 else { return nil }
            let raw = String(decoding: s[start..<min(i, end)], as: UTF8.self).trimmingCharacters(in: .whitespaces)
            if raw.isEmpty { return nil }
            if raw == "null" {
                out.append(nil)
            } else if raw.hasPrefix("\"") {
                guard let v = try? JSONSerialization.jsonObject(with: Data(raw.utf8), options: .fragmentsAllowed) as? String else { return nil }
                out.append(v)
            } else {
                out.append(raw)
            }
            i += 1 // the comma
        }
        return out
    }
}
