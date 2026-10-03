#if os(macOS)
import Foundation
import PostgresNIO
import Testing
@testable import TaisceKit
@testable import TaisceSQLPostgres

/// Postgres values as text, from hand-built binary cells (no server).
@Suite struct PostgresTextTests {
    func cell(_ type: PostgresDataType, _ write: (inout ByteBuffer) -> Void) -> PostgresCell {
        var b = ByteBuffer()
        write(&b)
        return PostgresCell(bytes: b, dataType: type, format: .binary, columnName: "c", columnIndex: 0)
    }

    @Test func commonTypes() {
        #expect(PostgresText.format(PostgresCell(bytes: nil, dataType: .int4, format: .binary, columnName: "c", columnIndex: 0)) == nil)
        #expect(PostgresText.format(cell(.int4) { $0.writeInteger(Int32(-42)) }) == "-42")
        #expect(PostgresText.format(cell(.int8) { $0.writeInteger(Int64(1) << 40) }) == "1099511627776")
        #expect(PostgresText.format(cell(.bool) { $0.writeInteger(UInt8(1)) }) == "t")
        #expect(PostgresText.format(cell(.text) { $0.writeString("héllo") }) == "héllo")
        #expect(PostgresText.format(cell(.varchar) { $0.writeString("v") }) == "v")
        #expect(PostgresText.format(cell(.float8) { $0.writeInteger((1.5 as Double).bitPattern) }) == "1.5")
        #expect(PostgresText.format(cell(.json) { $0.writeString(#"{"a":1}"#) }) == #"{"a":1}"#)
        #expect(PostgresText.format(cell(.jsonb) { $0.writeInteger(UInt8(1)); $0.writeString(#"{"a": 1}"#) }) == #"{"a": 1}"#)
        #expect(PostgresText.format(cell(.bytea) { $0.writeBytes([0xde, 0xad]) }) == "\\xdead")
        let u = UUID()
        #expect(PostgresText.format(cell(.uuid) { $0.writeBytes(withUnsafeBytes(of: u.uuid) { Array($0) }) }) == u.uuidString.lowercased())
        // text format (never asked for, but handled)
        #expect(PostgresText.format(PostgresCell(bytes: ByteBuffer(string: "7"), dataType: .int4, format: .text, columnName: "c", columnIndex: 0)) == "7")
    }

    @Test func datesAndTimes() {
        // 2000-01-01 is day 0
        #expect(PostgresText.format(cell(.date) { $0.writeInteger(Int32(0)) }) == "2000-01-01")
        #expect(PostgresText.format(cell(.date) { $0.writeInteger(Int32(9772)) }) == "2026-10-03")
        #expect(PostgresText.format(cell(.date) { $0.writeInteger(Int32(-1)) }) == "1999-12-31")
        #expect(PostgresText.format(cell(.date) { $0.writeInteger(Int32.max) }) == "infinity")
        let us = Int64(9772) * 86_400_000_000 + Int64(7 * 3600 + 54 * 60 + 17) * 1_000_000 + 250_000
        #expect(PostgresText.format(cell(.timestamp) { $0.writeInteger(us) }) == "2026-10-03 07:54:17.25")
        #expect(PostgresText.format(cell(.timestamptz) { $0.writeInteger(us) }) == "2026-10-03 07:54:17.25+00")
        #expect(PostgresText.format(cell(.timestamp) { $0.writeInteger(Int64(-1)) }) == "1999-12-31 23:59:59.999999")
        #expect(PostgresText.format(cell(.time) { $0.writeInteger(Int64(3_600_000_000)) }) == "01:00:00")
    }

    func numeric(_ digits: [Int16], weight: Int16, negative: Bool = false, scale: UInt16) -> String? {
        PostgresText.format(cell(.numeric) { b in
            b.writeInteger(Int16(digits.count))
            b.writeInteger(weight)
            b.writeInteger(UInt16(negative ? 0x4000 : 0))
            b.writeInteger(scale)
            for d in digits { b.writeInteger(d) }
        })
    }

    @Test func numerics() {
        #expect(numeric([1, 5000], weight: 0, scale: 2) == "1.50")
        #expect(numeric([3], weight: 0, negative: true, scale: 2) == "-3.00")
        #expect(numeric([], weight: 0, scale: 0) == "0")
        #expect(numeric([1, 2345, 6789], weight: 1, scale: 4) == "12345.6789")
        #expect(numeric([12], weight: -1, scale: 4) == "0.0012")
        #expect(numeric([1], weight: 2, scale: 0) == "100000000")
    }

    @Test func unknownTypesSaySo() {
        #expect(PostgresText.format(cell(.point) { $0.writeBytes([UInt8](repeating: 0, count: 16)) }) == "<point, 16 bytes>")
        #expect(PostgresText.format(cell(PostgresDataType(20000)) { $0.writeString("happy") }) == "happy")
        #expect(PostgresText.interval(months: 14, days: 3, us: 3_600_000_000) == "1 year 2 mons 3 days 01:00:00")
        #expect(PostgresText.interval(months: 0, days: 0, us: 0) == "00:00:00")
        #expect(PostgresText.typeName(.int4) == "integer")
        #expect(PostgresText.array(["a", "b c", "", "NULL"]) == #"{a,"b c","","NULL"}"#)
    }

    @Test func readOnlyIsAStartupParameter() throws {
        #expect(PostgresDriver.startupParameters(allowWrites: false).contains { $0 == ("default_transaction_read_only", "on") })
        #expect(!PostgresDriver.startupParameters(allowWrites: true).contains { $0.0 == "default_transaction_read_only" })
        var s = DataSource(name: "pg", kind: .postgres)
        s.host = " db.example "
        s.user = "me"
        s.tlsMode = .disable
        let d = PostgresDriver(s, password: "pw")
        #expect(d.host == "db.example")
        let c = try d.configuration()
        #expect(c.host == "db.example")
        #expect(c.port == 5432)
        #expect(c.database == nil)
        #expect(!c.tls.isAllowed)
        #expect(c.options.additionalStartupParameters.contains { $0 == ("default_transaction_read_only", "on") })
        s.tlsMode = .require
        #expect(try PostgresDriver(s, password: nil).configuration().tls.isEnforced)
        s.tlsMode = .verifyFull
        #expect(try PostgresDriver(s, password: nil).configuration().tls.isEnforced)
        // Prefer only to this Mac
        s.tlsMode = .prefer
        #expect(throws: SQLDriverError.self) { try PostgresDriver(s, password: nil).configuration() }
        s.host = "localhost"
        let prefer = try PostgresDriver(s, password: nil).configuration().tls
        #expect(prefer.isAllowed && !prefer.isEnforced)
    }
}

/// Against real servers, when given: `TAISCE_SQL_PG=host:port` (user
/// `postgres`, password `TAISCE_SQL_PG_PASSWORD`, database `taisce`) and
/// `TAISCE_SQL_CH=http://host:8123` (user `taisce`, password
/// `TAISCE_SQL_CH_PASSWORD`, database `taisce`). `apple/scripts/sql-servers.sh`
/// starts both in containers and prints the variables.
enum SQLServers {
    static let env = ProcessInfo.processInfo.environment
    static var pg: (host: String, port: Int)? {
        guard let v = env["TAISCE_SQL_PG"], let colon = v.lastIndex(of: ":"), let port = Int(v[v.index(after: colon)...]) else { return nil }
        return (String(v[..<colon]), port)
    }
    static var ch: URL? { env["TAISCE_SQL_CH"].flatMap(URL.init(string:)) }

    static func postgres(writes: Bool) -> PostgresDriver {
        PostgresDriver(host: pg!.host, port: pg!.port, database: "taisce", user: "postgres", password: env["TAISCE_SQL_PG_PASSWORD"], tlsMode: .disable, allowWrites: writes)
    }

    static func clickhouse(writes: Bool) -> ClickHouseDriver {
        ClickHouseDriver(url: ch!, database: "taisce", user: "taisce", password: env["TAISCE_SQL_CH_PASSWORD"], allowWrites: writes)
    }
}

@Suite(.serialized, .enabled(if: SQLServers.pg != nil)) struct PostgresServerTests {
    @Test func readOnlyByDefaultWritesWhenAllowed() async throws {
        let table = "taisce_t_\(UUID().uuidString.prefix(8).lowercased())"
        let rw = SQLServers.postgres(writes: true)
        let made = try await rw.run("CREATE TABLE \(table) (n int, s text, d numeric(10,2), at timestamptz); INSERT INTO \(table) VALUES (1, 'a', 1.50, '2026-10-03 07:54:17+00'), (2, NULL, -3, NULL)", cap: 10)
        #expect(made.map(\.outcome) == [.done(rowsAffected: nil), .done(rowsAffected: 2)])
        defer { Task { _ = try? await rw.run("DROP TABLE \(table)", cap: 1) } }

        let ro = SQLServers.postgres(writes: false)
        #expect(try await ro.testConnection().hasSuffix("read-only"))
        let refused = try await ro.run("INSERT INTO \(table) VALUES (3)", cap: 10)
        #expect(refused.last?.error?.contains("read-only transaction") == true)
        let rows = try await ro.run("SELECT * FROM \(table) ORDER BY n", cap: 10)
        #expect(rows.last?.outcome == .rows(SQLResultSet(
            columns: [SQLColumn("n", type: "integer"), SQLColumn("s", type: "text"), SQLColumn("d", type: "numeric"), SQLColumn("at", type: "timestamptz")],
            rows: [["1", "a", "1.50", "2026-10-03 07:54:17+00"], ["2", nil, "-3.00", nil]]
        )))
        let none = try await ro.run("SELECT * FROM \(table) WHERE false", cap: 10)
        #expect(none.last?.outcome == .rows(SQLResultSet(columns: [])))
    }

    /// Statements that would end or reopen the read-only transaction are
    /// refused (by the server or the driver's check) and write nothing.
    @Test func readOnlyCantBeUndone() async throws {
        let table = "taisce_e_\(UUID().uuidString.prefix(8).lowercased())"
        let rw = SQLServers.postgres(writes: true)
        _ = try await rw.run("CREATE TABLE \(table) (n int)", cap: 1)
        defer { Task { _ = try? await rw.run("DROP TABLE \(table)", cap: 1) } }
        let ro = SQLServers.postgres(writes: false)
        let escapes = [
            "BEGIN READ WRITE; INSERT INTO \(table) VALUES (1)",
            "SET default_transaction_read_only = off; INSERT INTO \(table) VALUES (2)",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE; INSERT INTO \(table) VALUES (3)",
            "SET TRANSACTION READ WRITE; INSERT INTO \(table) VALUES (4)",
            "DO $$ BEGIN COMMIT; INSERT INTO \(table) VALUES (5); COMMIT; END $$",
            "COMMIT; BEGIN READ WRITE; INSERT INTO \(table) VALUES (6); COMMIT",
            "ROLLBACK; SET default_transaction_read_only = off; INSERT INTO \(table) VALUES (7)",
            "COMMIT AND CHAIN; INSERT INTO \(table) VALUES (8)",
            "SET default_transaction_read_only = off; COMMIT; INSERT INTO \(table) VALUES (9)",
        ]
        for sql in escapes {
            let r = try await ro.run(sql, cap: 1)
            #expect(r.last?.error != nil, "\(sql)")
            #expect(!r.contains { $0.sql.hasPrefix("INSERT") && $0.error == nil }, "\(sql)")
        }
        let caught = try await ro.run("COMMIT; INSERT INTO \(table) VALUES (10)", cap: 1)
        #expect(caught.count == 1 && caught[0].error?.contains("read-only transaction the block runs in") == true)
        let n = try await rw.run("SELECT count(*) FROM \(table)", cap: 1)
        #expect(n.last?.outcome == .rows(SQLResultSet(columns: [SQLColumn("count", type: "bigint")], rows: [["0"]])))
        // and an ordinary read-only run still works, temp state and all
        let ok = try await ro.run("SET search_path = public; SELECT count(*) FROM \(table)", cap: 1)
        #expect(ok.allSatisfy { $0.error == nil } && ok.count == 2)
    }

    @Test func multiStatementCapAndErrors() async throws {
        let ro = SQLServers.postgres(writes: false)
        let r = try await ro.run("SELECT 1 AS a; SELECT g FROM generate_series(1, 1500) g", cap: 1000)
        guard case .rows(let set) = r.last?.outcome else { Issue.record("no rows"); return }
        #expect(set.rows.count == 1000)
        #expect(set.totalRows == 1500)
        let bad = try await ro.run("SELECT 1; SELEC 2; SELECT 3", cap: 10)
        #expect(bad.count == 2)
        #expect(bad.last?.error?.contains("syntax error") == true)
    }

    @Test func stopCancelsTheServerQuery() async throws {
        let ro = SQLServers.postgres(writes: false)
        let task = Task { await SQLRunner.run(ro, sql: "SELECT pg_sleep(30)") }
        try await Task.sleep(for: .milliseconds(800))
        let t0 = ContinuousClock.now
        task.cancel()
        let outcome = await task.value
        #expect(outcome.stopped)
        #expect(ContinuousClock.now - t0 < .seconds(5))
        // the server let go of it
        try await Task.sleep(for: .milliseconds(500))
        let left = try await ro.run("SELECT count(*) FROM pg_stat_activity WHERE query = 'SELECT pg_sleep(30)' AND state = 'active'", cap: 1)
        #expect(left.last?.outcome == .rows(SQLResultSet(columns: [SQLColumn("count", type: "bigint")], rows: [["0"]])))
    }

    @Test func badPasswordIsAMessage() async throws {
        var d = SQLServers.postgres(writes: false)
        d.password = "wrong"
        let outcome = await SQLRunner.run(d, sql: "SELECT 1")
        #expect(outcome.message?.contains("password") == true)
    }
}

@Suite(.serialized, .enabled(if: SQLServers.ch != nil)) struct ClickHouseServerTests {
    @Test func readOnlyByDefaultWritesWhenAllowed() async throws {
        let table = "taisce_t_\(UUID().uuidString.prefix(8).lowercased())"
        let rw = SQLServers.clickhouse(writes: true)
        let made = try await rw.run("CREATE TABLE \(table) (n UInt32, s Nullable(String)) ENGINE = Memory; INSERT INTO \(table) VALUES (1, 'a'), (2, NULL)", cap: 10)
        #expect(made.allSatisfy { $0.error == nil })
        defer { Task { _ = try? await rw.run("DROP TABLE \(table)", cap: 1) } }

        let ro = SQLServers.clickhouse(writes: false)
        #expect(try await ro.testConnection().hasSuffix("read-only"))
        let refused = try await ro.run("INSERT INTO \(table) VALUES (3, 'c')", cap: 10)
        #expect(refused.last?.error?.contains("readonly") == true)
        let unset = try await ro.run("SET readonly = 0; INSERT INTO \(table) VALUES (3, 'c')", cap: 10)
        #expect(unset.count == 1)
        #expect(unset.last?.error?.contains("readonly") == true)
        let sneaky = try await ro.run("INSERT INTO \(table) SETTINGS readonly = 0 VALUES (3, 'c')", cap: 10)
        #expect(sneaky.last?.error != nil)
        // readonly=1: a query's own SETTINGS are refused too (the price of
        // refusing table functions, below)
        let settings = try await ro.run("SELECT count() FROM numbers(10) SETTINGS max_block_size = 1", cap: 10)
        #expect(settings.last?.error?.contains("readonly") == true)
        let rows = try await ro.run("SELECT * FROM \(table) ORDER BY n", cap: 10)
        #expect(rows.last?.outcome == .rows(SQLResultSet(
            columns: [SQLColumn("n", type: "UInt32"), SQLColumn("s", type: "Nullable(String)")],
            rows: [["1", "a"], ["2", nil]]
        )))
    }

    /// Table functions reach other servers and files with the server's own
    /// access: read-only refuses them for access, before any connection is
    /// tried (readonly=2 tried to connect: "Connection refused").
    @Test func tableFunctionsAreRefusedReadOnly() async throws {
        let ro = SQLServers.clickhouse(writes: false)
        for sql in [
            "SELECT * FROM url('http://127.0.0.1:1/x', 'CSV', 'a String')",
            "SELECT * FROM remote('127.0.0.1:1', system.one)",
            "SELECT * FROM s3('http://127.0.0.1:1/bucket/dir/x.csv', 'CSV', 'a String')",
            "SELECT * FROM file('x.csv', 'CSV', 'a String')",
            "SELECT * FROM postgresql('127.0.0.1:1', 'd', 't', 'u', 'p')",
        ] {
            let r = try await ro.run(sql, cap: 1)
            let e = r.last?.error ?? ""
            #expect(e.contains("(READONLY)"), "\(sql): \(e)")
            #expect(!e.contains("onnection refused") && !e.contains("NETWORK_ERROR"), "\(sql): \(e)")
        }
    }

    @Test func capErrorsAndStop() async throws {
        let ro = SQLServers.clickhouse(writes: false)
        let r = try await ro.run("SELECT 1; SELECT number FROM numbers(1500)", cap: 1000)
        guard case .rows(let set) = r.last?.outcome else { Issue.record("no rows"); return }
        #expect(set.rows.count == 1000)
        #expect(set.totalRows == 1500)
        let bad = try await ro.run("SELECT 1; SELEC 2", cap: 10)
        #expect(bad.last?.error?.contains("Syntax error") == true)

        let task = Task { await SQLRunner.run(ro, sql: "SELECT sum(number) FROM numbers(100000000000)") }
        try await Task.sleep(for: .milliseconds(800))
        task.cancel()
        let outcome = await task.value
        #expect(outcome.stopped)
    }
}
#endif
