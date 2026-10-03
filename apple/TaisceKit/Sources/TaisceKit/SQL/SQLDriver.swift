import Foundation

/// Which fences are SQL, and which kind of source they need.
public struct SQLFence: Sendable, Hashable {
    /// nil: any source (`sql`)
    public var requiredKind: DataSourceKind?

    /// `sql`, or an alias that names the database: `sqlite`, `postgres` /
    /// `postgresql`, `clickhouse`.
    public init?(_ language: String?) {
        switch language?.lowercased() {
        case "sql": requiredKind = nil
        case "sqlite", "sqlite3": requiredKind = .sqlite
        case "postgres", "postgresql", "psql": requiredKind = .postgres
        case "clickhouse": requiredKind = .clickhouse
        default: return nil
        }
    }

    public func accepts(_ source: DataSource) -> Bool {
        requiredKind == nil || requiredKind == source.kind
    }
}

/// One database connection's worth of work: run a block's SQL, statement
/// by statement, honouring Task cancellation (Stop, the timeout).
///
/// Read-only is the database's job, never SQL parsing: SQLite opens the
/// file read-only, Postgres starts a read-only session, ClickHouse gets
/// `readonly=1`, unless the source allows writes.
public protocol SQLDriver: Sendable {
    /// Statements in order, stopping at the first error (the last element is
    /// then `.failed`). Throws only when nothing could run at all (no
    /// connection, bad credentials), or `CancellationError`.
    func run(_ sql: String, cap: Int) async throws -> [SQLStatementResult]

    /// Connect and ask something trivial; a line about the server on success.
    func testConnection() async throws -> String
}

public struct SQLDriverError: Error, LocalizedError, Sendable, Hashable {
    public var message: String
    public init(_ message: String) { self.message = message }
    public var errorDescription: String? { message }
}

public enum SQLRunner {
    public static let timeout: Duration = .seconds(300)

    /// Run `sql` on `driver`, bounded by `timeout`. Cancelling the calling
    /// task is Stop. Never throws: what went wrong is in the outcome.
    public static func run(_ driver: any SQLDriver, sql: String, cap: Int = SQLResultSet.defaultCap, timeout: Duration = Self.timeout) async -> SQLRunOutcome {
        let start = ContinuousClock.now
        enum Race: Sendable {
            case done([SQLStatementResult])
            case failed(String)
            case cancelled
            case timedOut
        }
        let result: Race = await withTaskGroup(of: Race.self) { group in
            group.addTask {
                do {
                    return .done(try await driver.run(sql, cap: cap))
                } catch is CancellationError {
                    return .cancelled
                } catch {
                    return Task.isCancelled ? .cancelled : .failed(Self.describe(error))
                }
            }
            group.addTask {
                do {
                    try await Task.sleep(for: timeout)
                    return .timedOut
                } catch {
                    return .cancelled
                }
            }
            let first = await group.next() ?? .cancelled
            group.cancelAll()
            // wait for the driver to wind down (it was cancelled): nothing
            // of this run outlives it
            for await _ in group {}
            return first
        }
        var out = SQLRunOutcome(duration: ContinuousClock.now - start)
        switch result {
        case .done(let s):
            out.statements = s
            if Task.isCancelled { out.stopped = true }
        case .failed(let m): out.message = m
        case .cancelled: out.stopped = true
        case .timedOut: out.timedOut = true
        }
        return out
    }

    /// An error as one readable line.
    public static func describe(_ error: any Error) -> String {
        if let e = error as? LocalizedError, let d = e.errorDescription { return d }
        if let e = error as? URLError { return e.localizedDescription }
        let s = String(describing: error)
        return s.isEmpty ? error.localizedDescription : s
    }
}

/// Splits a script into statements at top-level `;`, skipping quotes,
/// comments and (Postgres) dollar quoting. Statements that are only
/// whitespace and comments are dropped. Pure.
///
/// Only for running statements one by one: never a read-only check.
public enum SQLStatements {
    public struct Dialect: Sendable {
        public var dollarQuotes = false
        public var backslashEscapes = false
        public var nestedBlockComments = false

        public static let postgres = Dialect(dollarQuotes: true, nestedBlockComments: true)
        public static let clickhouse = Dialect(backslashEscapes: true)
        public static let standard = Dialect()
    }

    public static func split(_ sql: String, dialect: Dialect = .standard) -> [String] {
        let chars = Array(sql.unicodeScalars)
        var out: [String] = []
        var cur = String.UnicodeScalarView()
        var hasCode = false
        var i = 0
        func flush() {
            let s = String(cur).trimmingCharacters(in: .whitespacesAndNewlines)
            if hasCode, !s.isEmpty { out.append(s) }
            cur = String.UnicodeScalarView()
            hasCode = false
        }
        func at(_ k: Int) -> Unicode.Scalar? { k < chars.count ? chars[k] : nil }
        while i < chars.count {
            let c = chars[i]
            switch c {
            case ";":
                flush()
                i += 1
                continue
            case "-" where at(i + 1) == "-":
                while i < chars.count, chars[i] != "\n" { cur.append(chars[i]); i += 1 }
                continue
            case "/" where at(i + 1) == "*":
                var depth = 0
                repeat {
                    if chars[i] == "/", at(i + 1) == "*" {
                        depth += 1
                        cur.append(chars[i]); cur.append(chars[i + 1]); i += 2
                        if !dialect.nestedBlockComments, depth > 1 { depth = 1 }
                    } else if chars[i] == "*", at(i + 1) == "/" {
                        depth -= 1
                        cur.append(chars[i]); cur.append(chars[i + 1]); i += 2
                    } else {
                        cur.append(chars[i]); i += 1
                    }
                } while depth > 0 && i < chars.count
                continue
            case "'", "\"", "`":
                hasCode = true
                cur.append(c)
                i += 1
                while i < chars.count {
                    let d = chars[i]
                    cur.append(d)
                    i += 1
                    if dialect.backslashEscapes, d == "\\", i < chars.count {
                        cur.append(chars[i]); i += 1
                        continue
                    }
                    if d == c {
                        // a doubled quote is an escaped one
                        if at(i) == c { cur.append(c); i += 1; continue }
                        break
                    }
                }
                continue
            case "$" where dialect.dollarQuotes:
                if let tag = dollarTag(chars, i) {
                    hasCode = true
                    let t = Array(tag.unicodeScalars)
                    for s in t { cur.append(s) }
                    i += t.count
                    while i < chars.count {
                        if chars[i] == "$", Array(chars[i..<min(chars.count, i + t.count)]) == t {
                            for s in t { cur.append(s) }
                            i += t.count
                            break
                        }
                        cur.append(chars[i]); i += 1
                    }
                    continue
                }
            default:
                break
            }
            if !c.properties.isWhitespace { hasCode = true }
            cur.append(c)
            i += 1
        }
        flush()
        return out
    }

    /// The statement without the comments before it, trimmed (for showing it).
    public static func droppingLeadingComments(_ s: String) -> String {
        var rest = Substring(s)
        while true {
            rest = rest.drop { $0.isWhitespace }
            if rest.hasPrefix("--") {
                rest = rest.drop { $0 != "\n" }
            } else if rest.hasPrefix("/*"), let end = rest.range(of: "*/") {
                rest = rest[end.upperBound...]
            } else {
                break
            }
        }
        return rest.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// `$$` or `$tag$` starting at `i` (a tag is an identifier).
    static func dollarTag(_ chars: [Unicode.Scalar], _ i: Int) -> String? {
        // `$1` is a parameter, not a quote
        var j = i + 1
        var tag = "$"
        while j < chars.count, chars[j] != "$" {
            let c = chars[j]
            let ok = c == "_" || c.properties.isAlphabetic || (j > i + 1 && ("0"..."9").contains(c))
            guard ok else { return nil }
            tag.unicodeScalars.append(c)
            j += 1
        }
        guard j < chars.count else { return nil }
        return tag + "$"
    }
}
