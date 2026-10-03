import Foundation
import SQLite3
import Synchronization

/// A SQLite file through the system's libsqlite3. Without Allow writes the
/// file is opened `SQLITE_OPEN_READONLY`, so SQLite itself refuses any
/// write (and an ATTACH inherits it). Never creates a file.
public struct SQLiteDriver: SQLDriver {
    public var path: String
    public var allowWrites: Bool

    public init(path: String, allowWrites: Bool) {
        self.path = path
        self.allowWrites = allowWrites
    }

    public init(_ source: DataSource) {
        self.init(path: source.path, allowWrites: source.allowWrites)
    }

    var expandedPath: String { FenceInfo.expandTilde(path.trimmingCharacters(in: .whitespaces), home: NSHomeDirectory()) }

    public func run(_ sql: String, cap: Int) async throws -> [SQLStatementResult] {
        let conn = try SQLiteConnection(path: expandedPath, writable: allowWrites)
        return try await conn.onBackground { try $0.execute(sql, cap: cap) }
    }

    public func testConnection() async throws -> String {
        let conn = try SQLiteConnection(path: expandedPath, writable: allowWrites)
        let version = try await conn.onBackground { c -> String in
            // reads the header: "file is not a database" shows up here
            _ = try c.execute("PRAGMA schema_version", cap: 1)
            let r = try c.execute("SELECT sqlite_version()", cap: 1)
            if case .rows(let set) = r.last?.outcome, let v = set.rows.first?.first ?? nil { return v }
            if let e = r.last?.error { throw SQLDriverError(e) }
            return "?"
        }
        return "SQLite \(version)\(allowWrites ? "" : ", read-only")"
    }
}

/// One open database handle. Closed when the work finishes; `interrupt()`
/// (from Stop or the timeout) is safe from any thread until then.
final class SQLiteConnection: Sendable {
    private struct Handle: @unchecked Sendable { var db: OpaquePointer? }
    private let handle: Mutex<Handle>
    private let cancelled = Atomic<Bool>(false)

    init(path: String, writable: Bool) throws {
        var db: OpaquePointer?
        let flags = (writable ? SQLITE_OPEN_READWRITE : SQLITE_OPEN_READONLY) | SQLITE_OPEN_FULLMUTEX
        let rc = sqlite3_open_v2(path, &db, flags, nil)
        guard rc == SQLITE_OK, let db else {
            let msg = db.map { String(cString: sqlite3_errmsg($0)) } ?? String(cString: sqlite3_errstr(rc))
            sqlite3_close_v2(db)
            if !FileManager.default.fileExists(atPath: path) { throw SQLDriverError("No database file at \(path)") }
            throw SQLDriverError("Couldn't open \(path): \(msg)")
        }
        sqlite3_busy_timeout(db, 5000)
        sqlite3_extended_result_codes(db, 1)
        handle = Mutex(Handle(db: db))
    }

    deinit {
        close()
    }

    func close() {
        handle.withLock { h in
            if let db = h.db { sqlite3_close_v2(db) }
            h.db = nil
        }
    }

    func interrupt() {
        cancelled.store(true, ordering: .sequentiallyConsistent)
        handle.withLock { h in
            if let db = h.db { sqlite3_interrupt(db) }
        }
    }

    var isCancelled: Bool { cancelled.load(ordering: .sequentiallyConsistent) }

    /// `body` on a background thread (SQLite blocks), the handle closed
    /// after; Task cancellation interrupts it.
    func onBackground<T: Sendable>(_ body: @escaping @Sendable (SQLiteConnection) throws -> T) async throws -> T {
        let conn = self
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<T, any Error>) in
                DispatchQueue.global(qos: .userInitiated).async {
                    let r = Result { try body(conn) }
                    conn.close()
                    if conn.isCancelled { cont.resume(throwing: CancellationError()) } else { cont.resume(with: r) }
                }
            }
        } onCancel: {
            conn.interrupt()
        }
    }

    private var db: OpaquePointer? { handle.withLock { $0.db } }

    /// Every statement in `sql`, in order, stopping at the first error.
    func execute(_ sql: String, cap: Int) throws -> [SQLStatementResult] {
        guard let db else { throw SQLDriverError("The database is closed") }
        var results: [SQLStatementResult] = []
        let bytes = Array(sql.utf8CString)
        try bytes.withUnsafeBufferPointer { buf in
            guard let base = buf.baseAddress else { return }
            var tail: UnsafePointer<CChar>? = base
            while let cur = tail, cur.pointee != 0 {
                if isCancelled { throw CancellationError() }
                var stmt: OpaquePointer?
                var next: UnsafePointer<CChar>?
                let rc = sqlite3_prepare_v2(db, cur, -1, &stmt, &next)
                if rc != SQLITE_OK {
                    if isCancelled { throw CancellationError() }
                    let rest = String(cString: cur).trimmingCharacters(in: .whitespacesAndNewlines)
                    results.append(SQLStatementResult(sql: Self.firstStatementGuess(rest), outcome: .failed(String(cString: sqlite3_errmsg(db)))))
                    sqlite3_finalize(stmt)
                    return
                }
                tail = next
                guard let stmt else { continue } // only whitespace or a comment
                defer { sqlite3_finalize(stmt) }
                let text = SQLStatements.droppingLeadingComments(sqlite3_sql(stmt).map { String(cString: $0) } ?? "")
                let outcome = try step(stmt, db: db, cap: cap)
                results.append(SQLStatementResult(sql: text, outcome: outcome))
                if case .failed = outcome { return }
            }
        }
        return results
    }

    private func step(_ stmt: OpaquePointer, db: OpaquePointer, cap: Int) throws -> SQLStatementResult.Outcome {
        let n = Int(sqlite3_column_count(stmt))
        let columns = (0..<n).map { i in
            SQLColumn(
                sqlite3_column_name(stmt, Int32(i)).map { String(cString: $0) } ?? "?",
                type: sqlite3_column_decltype(stmt, Int32(i)).map { String(cString: $0) }
            )
        }
        var rows = SQLRowCollector(cap: cap)
        while true {
            if isCancelled { throw CancellationError() }
            let rc = sqlite3_step(stmt)
            if rc == SQLITE_ROW {
                if rows.isFull { rows.count() } else { rows.add(Self.row(stmt, n)) }
            } else if rc == SQLITE_DONE {
                break
            } else {
                if isCancelled { throw CancellationError() }
                return .failed(String(cString: sqlite3_errmsg(db)))
            }
        }
        if n > 0 { return .rows(rows.result(columns)) }
        return .done(rowsAffected: sqlite3_stmt_readonly(stmt) != 0 ? nil : Int(sqlite3_changes64(db)))
    }

    static func row(_ stmt: OpaquePointer, _ n: Int) -> [String?] {
        (0..<n).map { i -> String? in
            let c = Int32(i)
            switch sqlite3_column_type(stmt, c) {
            case SQLITE_NULL:
                return nil
            case SQLITE_BLOB:
                let len = Int(sqlite3_column_bytes(stmt, c))
                guard len > 0, let p = sqlite3_column_blob(stmt, c) else { return "x''" }
                let shown = UnsafeRawBufferPointer(start: p, count: min(len, 32))
                let hex = shown.map { String(format: "%02x", $0) }.joined()
                return len > 32 ? "x'\(hex)…' (\(len) bytes)" : "x'\(hex)'"
            default:
                guard let t = sqlite3_column_text(stmt, c) else { return "" }
                return String(cString: t)
            }
        }
    }

    /// For an error before a statement could be prepared: its first line.
    static func firstStatementGuess(_ rest: String) -> String {
        let s = SQLStatements.split(rest).first ?? rest
        return s.count > 200 ? String(s.prefix(200)) + "…" : s
    }
}
