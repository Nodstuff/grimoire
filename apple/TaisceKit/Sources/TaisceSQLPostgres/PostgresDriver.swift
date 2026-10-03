#if os(macOS) || targetEnvironment(macCatalyst)
import Foundation
import PostgresNIO
import Synchronization
import TaisceKit

/// Postgres through PostgresNIO, one connection per run. Without Allow
/// writes the whole run is one `BEGIN TRANSACTION READ ONLY` (the session
/// also starts with `default_transaction_read_only=on`), always rolled
/// back. A statement can't make it read-write once it has begun, but one
/// could end it (`COMMIT`, `ROLLBACK`) and the next could start another:
/// so after every statement the driver checks that it is still the same
/// transaction (`now()` unchanged) and still read-only, and otherwise rolls
/// back and stops the run. A role with only SELECT is still the real
/// boundary (`COPY … TO PROGRAM` needs superuser or
/// pg_execute_server_program; a read-only transaction doesn't stop it).
///
/// Statements run one by one on the same connection (the extended protocol
/// takes one statement per query). Rows stop being read at the first one
/// past the cap. A read-only run then cancels the statement on the server
/// (each statement runs after a `SAVEPOINT`, rolled back to after the
/// cancel, so the run goes on). With Allow writes the rest is discarded
/// unread as it arrives instead (a capped `INSERT … RETURNING` must still
/// finish), so a huge result there takes as long as it takes to send. Stop closes the connection and asks
/// the server to cancel the backend (`pg_cancel_backend`), best effort.
public struct PostgresDriver: SQLDriver {
    public var host: String
    public var port: Int
    public var database: String
    public var user: String
    public var password: String?
    public var tlsMode: PostgresTLSMode
    public var allowWrites: Bool

    public init(host: String, port: Int, database: String, user: String, password: String?, tlsMode: PostgresTLSMode, allowWrites: Bool) {
        self.host = host
        self.port = port
        self.database = database
        self.user = user
        self.password = password
        self.tlsMode = tlsMode
        self.allowWrites = allowWrites
    }

    public init(_ source: DataSource, password: String?) {
        self.init(
            host: source.host.trimmingCharacters(in: .whitespaces), port: source.port,
            database: source.database.trimmingCharacters(in: .whitespaces), user: source.user.trimmingCharacters(in: .whitespaces),
            password: password, tlsMode: source.tlsMode, allowWrites: source.allowWrites
        )
    }

    static let logger = Logger(label: "ie.null.taisce.sql.postgres")

    /// The connection settings (pure but for building the TLS context).
    public func configuration() throws -> PostgresConnection.Configuration {
        if tlsMode == .prefer, !DataSource.isLoopback(host) {
            throw SQLDriverError("TLS Prefer is only for a server on this Mac: choose Require and verify for \(host).")
        }
        let tls: PostgresConnection.Configuration.TLS
        switch tlsMode {
        case .disable:
            tls = .disable
        case .prefer, .require:
            var c = TLSConfiguration.makeClientConfiguration()
            c.certificateVerification = .none
            let ctx = try NIOSSLContext(configuration: c)
            tls = tlsMode == .prefer ? .prefer(ctx) : .require(ctx)
        case .verifyFull:
            tls = .require(try NIOSSLContext(configuration: .makeClientConfiguration()))
        }
        var config = PostgresConnection.Configuration(
            host: host, port: port, username: user, password: password,
            database: database.isEmpty ? nil : database, tls: tls
        )
        config.options.additionalStartupParameters = Self.startupParameters(allowWrites: allowWrites)
        return config
    }

    public static func startupParameters(allowWrites: Bool) -> [(String, String)] {
        var p = [("application_name", "Taisce")]
        if !allowWrites { p.append(("default_transaction_read_only", "on")) }
        return p
    }

    func connect() async throws -> PostgresConnection {
        let config = try configuration()
        do {
            return try await PostgresConnection.connect(configuration: config, id: Int.random(in: 1...Int.max), logger: Self.logger)
        } catch is CancellationError {
            throw CancellationError()
        } catch {
            throw SQLDriverError(Self.describe(error, connecting: "\(host):\(port)"))
        }
    }

    public func run(_ sql: String, cap: Int) async throws -> [SQLStatementResult] {
        let conn = try await connect()
        let backend = Mutex<Int?>(nil)
        let me = self
        return try await withTaskCancellationHandler {
            defer { Task.detached { try? await conn.close() } }
            if let pid = try? await Self.one(conn, "SELECT pg_backend_pid()").flatMap(Int.init) {
                backend.withLock { $0 = pid }
            }
            var results: [SQLStatementResult] = []
            var guardrail: ReadOnlyGuard?
            if !me.allowWrites {
                guardrail = try await ReadOnlyGuard.begin(conn)
            }
            for statement in SQLStatements.split(sql, dialect: .postgres) {
                try Task.checkCancellation()
                if guardrail != nil { _ = try await Self.one(conn, "SAVEPOINT taisce_statement") }
                // read-only, one past the cap: nothing was written, so stop
                // the server sending the rest
                let cancelAtCap = guardrail != nil ? backend.withLock { $0 } : nil
                let outcome = try await Self.runOne(conn, statement, cap: cap) {
                    if let cancelAtCap { await me.cancelBackend(cancelAtCap) }
                }
                if guardrail != nil, case .rows(let set) = outcome, set.isCapped {
                    _ = try? await Self.one(conn, "ROLLBACK TO SAVEPOINT taisce_statement")
                }
                if case .failed = outcome {
                    results.append(SQLStatementResult(sql: statement, outcome: outcome))
                    break
                }
                if let guardrail, let escaped = await guardrail.check(conn) {
                    results.append(SQLStatementResult(sql: statement, outcome: .failed(escaped)))
                    break
                }
                results.append(SQLStatementResult(sql: statement, outcome: outcome))
            }
            // stopped: the connection is closing, which rolls back anyway
            if guardrail != nil, !Task.isCancelled { _ = try? await Self.one(conn, "ROLLBACK") }
            return results
        } onCancel: {
            let pid = backend.withLock { $0 }
            Task.detached {
                if let pid { await me.cancelBackend(pid) }
                try? await conn.close()
            }
        }
    }

    /// The read-only run's transaction: begun by the driver, and checked
    /// after each statement to be the same one, still read-only.
    struct ReadOnlyGuard {
        /// `now()`: the transaction's start, fixed for its life
        let started: String

        static func begin(_ conn: PostgresConnection) async throws -> ReadOnlyGuard {
            do {
                _ = try await one(conn, "BEGIN TRANSACTION READ ONLY")
                guard let started = try await one(conn, "SELECT now()::text") else { throw SQLDriverError("Postgres didn't say when the transaction began") }
                return ReadOnlyGuard(started: started)
            } catch let e as SQLDriverError {
                throw e
            } catch {
                if Task.isCancelled { throw CancellationError() }
                throw SQLDriverError("Couldn't start a read-only transaction: \(describe(error, connecting: nil))")
            }
        }

        /// nil: still the driver's read-only transaction; else why the run
        /// stops (the transaction is then rolled back).
        func check(_ conn: PostgresConnection) async -> String? {
            let r = try? await conn.query(PostgresQuery(unsafeSQL: "SELECT now()::text, current_setting('transaction_read_only')"), logger: logger).get()
            let cells = r?.rows.first.map { row in row.map(PostgresText.format) }
            guard let cells, cells.count == 2 else {
                _ = try? await one(conn, "ROLLBACK")
                return Self.escaped
            }
            if cells[0] != started || cells[1] != "on" {
                _ = try? await one(conn, "ROLLBACK")
                return Self.escaped
            }
            return nil
        }

        static let escaped = "Stopped: this statement ended or changed the read-only transaction the block runs in, so the run was rolled back. Allow writes on the data source to run it."
    }

    /// A second connection asks the server to stop the first's query.
    func cancelBackend(_ pid: Int) async {
        guard let c = try? await connect() else { return }
        _ = try? await c.query(PostgresQuery(unsafeSQL: "SELECT pg_cancel_backend(\(pid))"), logger: Self.logger).get()
        try? await c.close()
    }

    static func one(_ conn: PostgresConnection, _ sql: String) async throws -> String? {
        let r = try await conn.query(PostgresQuery(unsafeSQL: sql), logger: logger).get()
        guard let cell = r.rows.first?.first(where: { _ in true }) else { return nil }
        return PostgresText.format(cell)
    }

    /// Thrown from the row handler at the first row past the cap: PostgresNIO
    /// then discards the rest unread (its query fails only once the server
    /// has finished sending, so `onCap` starts at once from the handler).
    struct CapReached: Error {}

    static func runOne(_ conn: PostgresConnection, _ statement: String, cap: Int, onCap: @escaping @Sendable () async -> Void = {}) async throws -> SQLStatementResult.Outcome {
        let collected = Mutex((rows: SQLRowCollector(cap: cap), columns: [SQLColumn]?.none, onCap: Task<Void, Never>?.none))
        do {
            let meta = try await conn.query(PostgresQuery(unsafeSQL: statement), logger: logger) { row in
                try collected.withLock { c in
                    if c.columns == nil {
                        c.columns = row.map { SQLColumn($0.columnName, type: PostgresText.typeName($0.dataType)) }
                    }
                    c.rows.add(row.map(PostgresText.format))
                    if c.rows.isDone {
                        if c.onCap == nil { c.onCap = Task.detached { await onCap() } }
                        throw CapReached()
                    }
                }
            }.get()
            let (rows, columns) = collected.withLock { ($0.rows, $0.columns) }
            if let columns { return .rows(rows.result(columns)) }
            // no rows: a SELECT still shows its (empty) result
            if ["SELECT", "SHOW", "FETCH", "VALUES", "TABLE"].contains(meta.command) { return .rows(SQLResultSet(columns: [])) }
            return .done(rowsAffected: meta.rows)
        } catch {
            let (rows, columns, capTask) = collected.withLock { ($0.rows, $0.columns, $0.onCap) }
            // done with it before the next statement (a cancel arriving
            // late would hit that one)
            await capTask?.value
            // past the cap the stream ends as PostgresNIO's queryCancelled,
            // or the server's query_canceled when onCap cancelled it
            let canceled = error is CapReached || (error as? PSQLError)?.code == .queryCancelled
                || (error as? PSQLError)?.serverInfo?[.sqlState] == "57014"
            if rows.isDone, canceled, !Task.isCancelled {
                return .rows(rows.result(columns ?? []))
            }
            return try fail(error)
        }
    }

    private static func fail(_ error: any Error) throws -> SQLStatementResult.Outcome {
        do {
            throw error
        } catch let e as PSQLError where e.code == .server {
            return .failed(describe(e, connecting: nil))
        } catch {
            if Task.isCancelled { throw CancellationError() }
            throw SQLDriverError(describe(error, connecting: nil))
        }
    }

    public func testConnection() async throws -> String {
        let conn = try await connect()
        defer { Task.detached { try? await conn.close() } }
        let version = (try? await Self.one(conn, "SHOW server_version")) ?? "?"
        let ro = (try? await Self.one(conn, "SHOW default_transaction_read_only")) == "on"
        return "Postgres \(version)\(ro ? ", read-only" : "")"
    }

    /// A readable line: the server's message (with its detail and hint), or
    /// what went wrong reaching it. PSQLError's own description is a
    /// generic "Database error" by design.
    static func describe(_ error: any Error, connecting: String?) -> String {
        if let e = error as? PSQLError {
            if let info = e.serverInfo, let msg = info[.message] {
                var s = msg
                if let d = info[.detail] { s += "\n" + d }
                if let h = info[.hint] { s += "\nHint: " + h }
                if let state = info[.sqlState] { s += " (SQLSTATE \(state))" }
                return s
            }
            let why = e.underlying.map { String(describing: $0) } ?? "\(e.code)"
            return connecting.map { "Couldn't connect to \($0): \(why)" } ?? why
        }
        let why = String(describing: error)
        return connecting.map { "Couldn't connect to \($0): \(why)" } ?? why
    }
}

/// Binary Postgres values as text, as `psql` would show them for the
/// common types; anything else says its type and size. Pure.
public enum PostgresText {
    public static func typeName(_ t: PostgresDataType) -> String {
        t.knownSQLName?.lowercased() ?? "oid \(t.rawValue)"
    }

    public static func format(_ cell: PostgresCell) -> String? {
        guard var bytes = cell.bytes else { return nil }
        if cell.format == .text { return bytes.readString(length: bytes.readableBytes) }
        switch cell.dataType {
        case .bool:
            return (try? cell.decode(Bool.self)).map { $0 ? "t" : "f" }
        case .int2, .int4, .int8, .oid:
            return (try? cell.decode(Int.self)).map(String.init)
        case .float4:
            return (try? cell.decode(Float.self)).map { "\($0)" }
        case .float8:
            return (try? cell.decode(Double.self)).map { "\($0)" }
        case .numeric:
            return numeric(&bytes)
        case .text, .varchar, .bpchar, .name, .char, .xml:
            return bytes.readString(length: bytes.readableBytes)
        case .interval:
            guard let us: Int64 = bytes.readInteger(), let days: Int32 = bytes.readInteger(), let months: Int32 = bytes.readInteger() else { return nil }
            return interval(months: Int(months), days: Int(days), us: us)
        case .uuid:
            return (try? cell.decode(UUID.self)).map { $0.uuidString.lowercased() }
        case .json:
            return bytes.readString(length: bytes.readableBytes)
        case .jsonb:
            bytes.moveReaderIndex(forwardBy: 1)
            return bytes.readString(length: bytes.readableBytes)
        case .bytea:
            let all = bytes.readableBytesView
            let hex = all.prefix(64).map { String(format: "%02x", $0) }.joined()
            return all.count > 64 ? "\\x\(hex)… (\(all.count) bytes)" : "\\x\(hex)"
        case .date:
            guard let days: Int32 = bytes.readInteger() else { return nil }
            if days == Int32.max { return "infinity" }
            if days == Int32.min { return "-infinity" }
            return day(Int64(days) * 86_400_000_000)
        case .timestamp, .timestamptz:
            guard let us: Int64 = bytes.readInteger() else { return nil }
            if us == Int64.max { return "infinity" }
            if us == Int64.min { return "-infinity" }
            return timestamp(us, zone: cell.dataType == .timestamptz)
        case .time:
            guard let us: Int64 = bytes.readInteger() else { return nil }
            return clock(us)
        case .textArray, .varcharArray, .bpcharArray, .nameArray:
            return (try? cell.decode([String].self)).map(array)
        case .int2Array, .int4Array, .int8Array:
            return (try? cell.decode([Int].self)).map { array($0.map(String.init)) }
        case .float8Array:
            return (try? cell.decode([Double].self)).map { array($0.map { "\($0)" }) }
        case .boolArray:
            return (try? cell.decode([Bool].self)).map { array($0.map { $0 ? "t" : "f" }) }
        case .uuidArray:
            return (try? cell.decode([UUID].self)).map { array($0.map { $0.uuidString.lowercased() }) }
        default:
            // enums and other user types send their label
            if cell.dataType.isUserDefined { return bytes.readString(length: bytes.readableBytes) }
            return "<\(typeName(cell.dataType)), \(bytes.readableBytes) bytes>"
        }
    }

    /// Microseconds since 2000-01-01 00:00 UTC, as `2026-10-03 07:54:17.25`.
    static func timestamp(_ us: Int64, zone: Bool) -> String {
        let (day, time) = splitDay(us)
        return dayString(day) + " " + clock(time) + (zone ? "+00" : "")
    }

    static func day(_ us: Int64) -> String { dayString(splitDay(us).day) }

    static func splitDay(_ us: Int64) -> (day: Int64, time: Int64) {
        let perDay: Int64 = 86_400_000_000
        var d = us / perDay
        var t = us % perDay
        if t < 0 { t += perDay; d -= 1 }
        return (d, t)
    }

    /// Days since 2000-01-01 as a proleptic Gregorian date.
    static func dayString(_ days: Int64) -> String {
        // civil-from-days (Howard Hinnant), shifted from 1970 to 2000
        let z = days + 10957 + 719_468
        let era = (z >= 0 ? z : z - 146_096) / 146_097
        let doe = z - era * 146_097
        let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100)
        let mp = (5 * doy + 2) / 153
        let d = doy - (153 * mp + 2) / 5 + 1
        let m = mp < 10 ? mp + 3 : mp - 9
        let y = yoe + era * 400 + (m <= 2 ? 1 : 0)
        return String(format: "%04lld-%02lld-%02lld", y, m, d)
    }

    static func clock(_ us: Int64) -> String {
        let s = us / 1_000_000
        let frac = us % 1_000_000
        var out = String(format: "%02lld:%02lld:%02lld", s / 3600, (s / 60) % 60, s % 60)
        if frac != 0 {
            var f = String(format: "%06lld", frac)
            while f.hasSuffix("0") { f.removeLast() }
            out += "." + f
        }
        return out
    }

    /// The binary numeric (base-10000 digits, weight, sign, display scale)
    /// as its exact decimal text, trailing zeros to the scale: `-3.00`.
    static func numeric(_ b: inout ByteBuffer) -> String? {
        guard let ndigits: Int16 = b.readInteger(), let weight: Int16 = b.readInteger(),
              let sign: UInt16 = b.readInteger(), let dscale: UInt16 = b.readInteger() else { return nil }
        switch sign {
        case 0xC000: return "NaN"
        case 0xD000: return "Infinity"
        case 0xF000: return "-Infinity"
        default: break
        }
        var digits: [Int] = []
        for _ in 0..<max(0, Int(ndigits)) {
            guard let d: Int16 = b.readInteger() else { return nil }
            digits.append(Int(d))
        }
        // digit i is worth 10000^(weight - i)
        var integer = ""
        if weight >= 0 {
            for i in 0...Int(weight) {
                let d = i < digits.count ? digits[i] : 0
                integer += integer.isEmpty ? String(d) : String(format: "%04d", d)
            }
        } else {
            integer = "0"
        }
        var fraction = ""
        var i = Int(weight) + 1
        while fraction.count < Int(dscale) {
            let d = i >= 0 && i < digits.count ? digits[i] : 0
            fraction += String(format: "%04d", d)
            i += 1
        }
        fraction = String(fraction.prefix(Int(dscale)))
        let negative = sign == 0x4000 && digits.contains { $0 != 0 }
        return (negative ? "-" : "") + integer + (fraction.isEmpty ? "" : "." + fraction)
    }

    /// As psql: `1 year 2 mons 3 days 04:05:06`.
    static func interval(months: Int, days: Int, us: Int64) -> String {
        var parts: [String] = []
        let y = months / 12, m = months % 12
        if y != 0 { parts.append("\(y) year\(abs(y) == 1 ? "" : "s")") }
        if m != 0 { parts.append("\(m) mon\(abs(m) == 1 ? "" : "s")") }
        if days != 0 { parts.append("\(days) day\(abs(days) == 1 ? "" : "s")") }
        if us != 0 || parts.isEmpty { parts.append((us < 0 ? "-" : "") + clock(abs(us))) }
        return parts.joined(separator: " ")
    }

    static func array(_ items: [String]) -> String {
        "{" + items.map { s in
            let needsQuotes = s.isEmpty || s.contains(where: { ",{}\" \\".contains($0) }) || s.uppercased() == "NULL"
            return needsQuotes ? "\"" + s.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\"" : s
        }.joined(separator: ",") + "}"
    }
}
#endif
