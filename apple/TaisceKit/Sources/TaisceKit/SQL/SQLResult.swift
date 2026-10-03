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
/// `cap` of them; `totalRows` counts every row the database sent.
public struct SQLResultSet: Sendable, Hashable {
    public static let defaultCap = 1000

    public var columns: [SQLColumn]
    public var rows: [[String?]]
    public var totalRows: Int

    public init(columns: [SQLColumn], rows: [[String?]] = [], totalRows: Int? = nil) {
        self.columns = columns
        self.rows = rows
        self.totalRows = totalRows ?? rows.count
    }

    /// Rows past the cap (counted, never kept).
    public var droppedRows: Int { max(0, totalRows - rows.count) }
    public var isCapped: Bool { droppedRows > 0 }

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
        if isCapped { lines.append("\n+\(droppedRows) more rows (capped)") }
        return lines.joined(separator: "\n") + "\n"
    }
}

/// Collects rows up to the cap and counts the rest.
public struct SQLRowCollector: Sendable {
    public let cap: Int
    public private(set) var rows: [[String?]] = []
    public private(set) var total = 0

    public init(cap: Int = SQLResultSet.defaultCap) {
        self.cap = cap
    }

    public mutating func add(_ row: @autoclosure () -> [String?]) {
        total += 1
        if rows.count < cap { rows.append(row()) }
    }

    /// Past the cap: the caller may skip decoding the row and just `count()`.
    public var isFull: Bool { rows.count >= cap }
    public mutating func count() { total += 1 }

    public func result(_ columns: [SQLColumn]) -> SQLResultSet {
        SQLResultSet(columns: columns, rows: rows, totalRows: total)
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
