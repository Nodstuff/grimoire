import Foundation

/// A result column: its name and, where the database says, its type.
public struct SQLColumn: Sendable, Hashable {
    public var name: String
    public var type: String?

    public init(_ name: String, type: String? = nil) {
        self.name = name
        self.type = type
    }
}

/// The rows a statement returned, as display text (nil = SQL NULL), at most
/// `cap` of them; `isCapped` when the database had more (the drivers stop
/// at the first row past the cap, so how many more is never known).
public struct SQLResultSet: Sendable, Hashable {
    public static let defaultCap = 1000

    public var columns: [SQLColumn]
    public var rows: [[String?]]
    /// there was at least one more row than `rows` holds
    public var isCapped: Bool

    public init(columns: [SQLColumn], rows: [[String?]] = [], isCapped: Bool = false) {
        self.columns = columns
        self.rows = rows
        self.isCapped = isCapped
    }

    /// "1,000+ rows" / "3 rows" / "1 row"
    public var rowCountText: String {
        let n = rows.count
        if isCapped { return "\(n.formatted())+ rows" }
        return n == 1 ? "1 row" : "\(n.formatted()) rows"
    }

    /// Tab-separated, a header line first; tabs and newlines in cells
    /// become spaces, NULL is empty.
    public var tsv: String {
        func cell(_ s: String?) -> String {
            (s ?? "").replacingOccurrences(of: "\t", with: " ").replacingOccurrences(of: "\r\n", with: " ").replacingOccurrences(of: "\n", with: " ")
        }
        var lines = [columns.map { cell($0.name) }.joined(separator: "\t")]
        for r in rows { lines.append(r.map(cell).joined(separator: "\t")) }
        return lines.joined(separator: "\n") + "\n"
    }

    /// A GFM table (pipes escaped, NULL shown as `NULL`), plus a line for
    /// rows past the cap.
    public var markdown: String {
        func cell(_ s: String?) -> String {
            guard let s else { return "NULL" }
            return s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "|", with: "\\|")
                .replacingOccurrences(of: "\r\n", with: "<br>").replacingOccurrences(of: "\n", with: "<br>")
        }
        guard !columns.isEmpty else { return "" }
        var lines = ["| " + columns.map { cell($0.name) }.joined(separator: " | ") + " |"]
        lines.append("|" + String(repeating: " --- |", count: columns.count))
        for r in rows { lines.append("| " + r.map(cell).joined(separator: " | ") + " |") }
        if isCapped { lines.append("\n(the first \(rows.count) rows; there are more)") }
        return lines.joined(separator: "\n") + "\n"
    }
}

/// Collects rows up to the cap; the row after that only marks the result
/// capped, and the driver stops reading (`isDone`).
public struct SQLRowCollector: Sendable {
    public let cap: Int
    public private(set) var rows: [[String?]] = []
    /// a row past the cap arrived
    public private(set) var isDone = false

    public init(cap: Int = SQLResultSet.defaultCap) {
        self.cap = cap
    }

    /// The row is only built while there is room for it.
    public mutating func add(_ row: @autoclosure () -> [String?]) {
        if rows.count < cap { rows.append(row()) } else { isDone = true }
    }

    public func result(_ columns: [SQLColumn]) -> SQLResultSet {
        SQLResultSet(columns: columns, rows: rows, isCapped: isDone)
    }
}

/// One statement of a run and what came of it.
public struct SQLStatementResult: Sendable, Hashable {
    public enum Outcome: Sendable, Hashable {
        case rows(SQLResultSet)
        /// a statement without a result set; rows changed when known
        case done(rowsAffected: Int?)
        case failed(String)
    }

    /// the statement's text, trimmed
    public var sql: String
    public var outcome: Outcome

    public init(sql: String, outcome: Outcome) {
        self.sql = sql
        self.outcome = outcome
    }

    public var error: String? {
        if case .failed(let e) = outcome { return e }
        return nil
    }
}

/// How a SQL run ended. Statements run in order and stop at the first
/// error, so a failed statement is always the last one.
public struct SQLRunOutcome: Sendable, Hashable {
    public var statements: [SQLStatementResult]
    public var duration: Duration
    public var timedOut = false
    public var stopped = false
    /// why it never ran or broke off: no connection, a bad password, …
    public var message: String?

    public init(statements: [SQLStatementResult] = [], duration: Duration = .zero, timedOut: Bool = false, stopped: Bool = false, message: String? = nil) {
        self.statements = statements
        self.duration = duration
        self.timedOut = timedOut
        self.stopped = stopped
        self.message = message
    }

    /// The result set shown: the last statement's, if it had rows.
    public var lastResultSet: SQLResultSet? {
        if case .rows(let r) = statements.last?.outcome { return r }
        return nil
    }

    /// The first failed statement (1-based position) and its error.
    public var failure: (index: Int, sql: String, error: String)? {
        guard let i = statements.firstIndex(where: { $0.error != nil }), let e = statements[i].error else { return nil }
        return (i + 1, statements[i].sql, e)
    }

    public var succeeded: Bool { message == nil && !timedOut && !stopped && failure == nil }
}
