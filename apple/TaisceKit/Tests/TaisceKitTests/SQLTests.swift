import Foundation
import Testing
@testable import TaisceKit

@Suite struct SQLFenceTests {
    @Test func languagesAndAliases() {
        #expect(SQLFence("sql")?.requiredKind == nil)
        #expect(SQLFence("SQL") != nil)
        #expect(SQLFence("sqlite")?.requiredKind == .sqlite)
        #expect(SQLFence("postgres")?.requiredKind == .postgres)
        #expect(SQLFence("postgresql")?.requiredKind == .postgres)
        #expect(SQLFence("clickhouse")?.requiredKind == .clickhouse)
        #expect(SQLFence("bash") == nil)
        #expect(SQLFence(nil) == nil)
        let pg = DataSource(name: "pg", kind: .postgres)
        #expect(SQLFence("sql")!.accepts(pg))
        #expect(SQLFence("postgres")!.accepts(pg))
        #expect(!SQLFence("sqlite")!.accepts(pg))
    }

    @Test func dbAttributeFromTheInfoString() {
        let f = FenceInfo("sql db=analytics")
        #expect(f.normalizedLanguage == "sql")
        #expect(f.attributes["db"] == "analytics")
        #expect(FenceInfo("sql DB=\"my db\"").attributes["db"] == "my db")
    }

    @Test func settingTheDbAttribute() {
        #expect(FenceEdit.settingAttribute("db", "prod", in: "```sql\nSELECT 1\n```") == "```sql db=prod\nSELECT 1\n```")
        // replaces, keeps other attributes and the fence's length and indent
        #expect(FenceEdit.settingAttribute("db", "b", in: "  ````sql db=a cwd=\"~/x y\"\nSELECT 1\n````") == "  ````sql db=b cwd=\"~/x y\"\nSELECT 1\n````")
        #expect(FenceEdit.settingAttribute("db", "a b", in: "```sql\nx\n```") == "```sql db=\"a b\"\nx\n```")
        // text around the fence stays
        #expect(FenceEdit.settingAttribute("db", "p", in: "Query:\n```sql\nx\n```\nafter") == "Query:\n```sql db=p\nx\n```\nafter")
        #expect(FenceEdit.settingAttribute("db", "p", in: "no fence") == nil)
        #expect(FenceEdit.settingAttribute("db", "p", in: "```\nx\n```") == nil)
    }
}

@Suite struct SQLStatementsTests {
    @Test func splitsAtTopLevelSemicolons() {
        #expect(SQLStatements.split("SELECT 1; SELECT 2;") == ["SELECT 1", "SELECT 2"])
        #expect(SQLStatements.split("SELECT 1") == ["SELECT 1"])
        #expect(SQLStatements.split("  ;; \n") == [])
        #expect(SQLStatements.split("SELECT 'a;b'; SELECT \"c;d\"") == ["SELECT 'a;b'", "SELECT \"c;d\""])
        #expect(SQLStatements.split("SELECT 'it''s; fine'; SELECT 2") == ["SELECT 'it''s; fine'", "SELECT 2"])
        #expect(SQLStatements.split("SELECT `a;b` FROM t") == ["SELECT `a;b` FROM t"])
    }

    @Test func commentsAreNotStatements() {
        #expect(SQLStatements.split("-- just a comment; really\n") == [])
        #expect(SQLStatements.split("SELECT 1; -- trailing; comment") == ["SELECT 1"])
        #expect(SQLStatements.split("/* a; b */ SELECT 1") == ["/* a; b */ SELECT 1"])
        #expect(SQLStatements.split("SELECT 1 -- x;\n, 2") == ["SELECT 1 -- x;\n, 2"])
        #expect(SQLStatements.droppingLeadingComments("  -- a\n/* b */\n SELECT 1 -- c\n") == "SELECT 1 -- c")
    }

    @Test func postgresDollarQuotesAndNestedComments() {
        let fn = "CREATE FUNCTION f() RETURNS int AS $$ BEGIN RETURN 1; END; $$ LANGUAGE plpgsql"
        #expect(SQLStatements.split(fn + "; SELECT f()", dialect: .postgres) == [fn, "SELECT f()"])
        let tagged = "SELECT $tag$ a;b $$ c; $tag$"
        #expect(SQLStatements.split(tagged + ";SELECT 2", dialect: .postgres) == [tagged, "SELECT 2"])
        // $1 is a parameter, not a quote
        #expect(SQLStatements.split("SELECT $1; SELECT 2", dialect: .postgres) == ["SELECT $1", "SELECT 2"])
        #expect(SQLStatements.split("/* a /* b; */ c; */ SELECT 1; SELECT 2", dialect: .postgres) == ["/* a /* b; */ c; */ SELECT 1", "SELECT 2"])
    }

    @Test func clickhouseBackslashEscapes() {
        #expect(SQLStatements.split("SELECT 'a\\';b'; SELECT 2", dialect: .clickhouse) == ["SELECT 'a\\';b'", "SELECT 2"])
    }
}

@Suite struct SQLResultTests {
    @Test func collectorCapsAndCounts() {
        var c = SQLRowCollector(cap: 2)
        for i in 0..<5 {
            if c.isFull { c.count() } else { c.add(["\(i)"]) }
        }
        let r = c.result([SQLColumn("n")])
        #expect(r.rows == [["0"], ["1"]])
        #expect(r.totalRows == 5)
        #expect(r.droppedRows == 3)
        #expect(r.isCapped)
    }

    @Test func tsvAndMarkdown() {
        let r = SQLResultSet(columns: [SQLColumn("a", type: "TEXT"), SQLColumn("b|c")], rows: [["x\ty", nil], ["1|2", "line\nbreak"]], totalRows: 4)
        #expect(r.tsv == "a\tb|c\nx y\t\n1|2\tline break\n")
        #expect(r.markdown == """
        | a | b\\|c |
        | --- | --- |
        | x\ty | NULL |
        | 1\\|2 | line<br>break |

        +2 more rows (capped)

        """)
        #expect(SQLResultSet(columns: []).markdown == "")
    }

    @Test func outcomeSummaries() {
        let ok = SQLRunOutcome(statements: [
            SQLStatementResult(sql: "SELECT 1", outcome: .rows(SQLResultSet(columns: [SQLColumn("1")], rows: [["1"]]))),
        ])
        #expect(ok.succeeded)
        #expect(ok.lastResultSet?.rows == [["1"]])
        let bad = SQLRunOutcome(statements: [
            SQLStatementResult(sql: "SELECT 1", outcome: .rows(SQLResultSet(columns: []))),
            SQLStatementResult(sql: "SELEC 2", outcome: .failed("syntax error")),
        ])
        #expect(!bad.succeeded)
        #expect(bad.lastResultSet == nil)
        #expect(bad.failure?.index == 2)
        #expect(bad.failure?.error == "syntax error")
    }
}

@Suite struct DataSourceTests {
    @Test func validation() {
        var s = DataSource(name: "", kind: .sqlite)
        #expect(s.problem(among: []) == "Give it a name.")
        s.name = "my db"
        #expect(s.problem(among: [])?.contains("letters, digits") == true)
        s.name = "local.db-1"
        #expect(s.problem(among: []) == "Choose the database file.")
        s.path = "~/x.db"
        #expect(s.problem(among: []) == nil)
        let other = DataSource(name: "LOCAL.db-1", kind: .postgres)
        #expect(s.problem(among: [other])?.contains("already") == true)
        // editing itself isn't a clash
        #expect(s.problem(among: [s]) == nil)

        var pg = DataSource(name: "pg", kind: .postgres)
        #expect(pg.problem(among: []) == "Enter the host.")
        pg.host = "db"
        #expect(pg.problem(among: []) == "Enter the user.")
        pg.user = "me"
        pg.port = 0
        #expect(pg.problem(among: [])?.contains("port") == true)
        pg.port = 5432
        #expect(pg.tlsMode == .verifyFull)
        #expect(pg.problem(among: []) == nil)
        #expect(pg.transportWarning == nil)
        #expect(pg.summary == "me@db:5432/")
        // Prefer (it can fall back to plain text) only for this Mac
        pg.tlsMode = .prefer
        #expect(pg.problem(among: [])?.contains("only for a server on this Mac") == true)
        for local in ["localhost", "127.0.0.1", "127.1.2.3", "::1", "[::1]", "db.localhost"] {
            pg.host = local
            #expect(pg.problem(among: []) == nil, "\(local)")
            #expect(pg.transportWarning != nil)
        }
        for remote in ["db", "10.0.0.1", "127.0.0.1.example.com", "128.0.0.1", "localhost.example.com"] {
            pg.host = remote
            #expect(pg.problem(among: []) != nil, "\(remote)")
        }
        // weaker modes are allowed anywhere, and the editor says what they give up
        pg.tlsMode = .disable
        #expect(pg.problem(among: []) == nil)
        #expect(pg.transportWarning?.contains("in the clear") == true)
        pg.tlsMode = .require
        #expect(pg.transportWarning?.contains("certificate isn't checked") == true)

        var ch = DataSource(name: "ch", kind: .clickhouse)
        #expect(ch.user == "default")
        ch.url = "host:8443"
        #expect(ch.problem(among: []) != nil)
        ch.url = "https://ch.example:8443"
        #expect(ch.problem(among: []) == nil)
        #expect(ch.transportWarning == nil)
        #expect(!ch.allowWrites)
        // http:// only to this Mac
        ch.url = "http://ch.example:8123"
        #expect(ch.problem(among: [])?.contains("Use https://") == true)
        #expect(throws: SQLDriverError.self) { try ClickHouseDriver(ch, password: "pw") }
        ch.url = "http://127.0.0.1:8123"
        #expect(ch.problem(among: []) == nil)
        #expect(ch.transportWarning?.contains("http://") == true)
        #expect((try? ClickHouseDriver(ch, password: nil))?.url.scheme == "http")
    }

    @Test func fileRoundTripAndDefaults() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("taisce-ds-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = DataSourceFile(url: dir.appendingPathComponent("datasources.json"))
        #expect(try file.load() == [])
        var b = DataSource(name: "b", kind: .postgres)
        b.host = "h"
        b.allowWrites = true
        b.tlsMode = .verifyFull
        let a = DataSource(name: "a", kind: .sqlite)
        try file.save([b, a])
        #expect(try file.load() == [a, b])
        let json = try String(contentsOf: file.url, encoding: .utf8)
        #expect(!json.contains("password"))
        // an older or hand-written file: missing fields default
        try Data(#"{"version":1,"sources":[{"id":"x","name":"n","kind":"clickhouse","url":"http://h:8123"}]}"#.utf8).write(to: file.url)
        let loaded = try #require(try file.load().first)
        #expect(loaded.url == "http://h:8123")
        #expect(!loaded.allowWrites)
        #expect(loaded.tlsMode == .verifyFull)
    }

    @Test func memorySecrets() throws {
        let s = MemoryDataSourceSecrets()
        try s.setPassword("pw", for: "1")
        #expect(try s.password(for: "1") == "pw")
        try s.setPassword("", for: "1")
        #expect(try s.password(for: "1") == nil)
    }
}

// MARK: SQLite

@Suite struct SQLiteDriverTests {
    /// A fresh database file with a table `t(n INTEGER, s TEXT, b BLOB, r REAL)`.
    static func makeDB(rows: Int = 3) async throws -> String {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("taisce-sqlite-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = dir.appendingPathComponent("test.db").path
        FileManager.default.createFile(atPath: path, contents: nil)
        let w = SQLiteDriver(path: path, allowWrites: true)
        let r = try await w.run("""
        CREATE TABLE t (n INTEGER, s TEXT, b BLOB, r REAL);
        WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < \(rows))
        INSERT INTO t SELECT x, 'row ' || x, NULL, x / 2.0 FROM c;
        UPDATE t SET s = NULL, b = x'00ff10' WHERE n = 2;
        """, cap: 10)
        #expect(r.allSatisfy { $0.error == nil })
        return path
    }

    static func cleanup(_ path: String) {
        try? FileManager.default.removeItem(atPath: (path as NSString).deletingLastPathComponent)
    }

    @Test func readOnlyRefusesWrites() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let ro = SQLiteDriver(path: path, allowWrites: false)
        let r = try await ro.run("INSERT INTO t (n) VALUES (99)", cap: 10)
        #expect(r.count == 1)
        #expect(r[0].error?.contains("readonly") == true)
        // nor sneaking round it
        let r2 = try await ro.run("PRAGMA query_only = 0; DELETE FROM t", cap: 10)
        #expect(r2.last?.error?.contains("readonly") == true)
        let attached = try await ro.run("ATTACH DATABASE '\(path)' AS other; INSERT INTO other.t (n) VALUES (1)", cap: 10)
        #expect(attached.last?.error != nil)
        let count = try await ro.run("SELECT count(*) AS n FROM t", cap: 10)
        #expect(count.last?.outcome == .rows(SQLResultSet(columns: [SQLColumn("n")], rows: [["3"]])))
    }

    /// `VACUUM INTO` writes a new file and `ATTACH` opens another database
    /// even on a read-only connection: both are refused, with or without
    /// Allow writes, and no file appears.
    @Test func vacuumIntoAndAttachAreRefused() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let dir = (path as NSString).deletingLastPathComponent
        // another database the app could reach
        let other = (dir as NSString).appendingPathComponent("other.db")
        FileManager.default.createFile(atPath: other, contents: nil)
        _ = try await SQLiteDriver(path: other, allowWrites: true).run("CREATE TABLE secret (x); INSERT INTO secret VALUES ('hidden')", cap: 1)
        for writes in [false, true] {
            let d = SQLiteDriver(path: path, allowWrites: writes)
            let out = (dir as NSString).appendingPathComponent("out-\(writes).db")
            let vacuum = try await d.run("VACUUM INTO '\(out)'", cap: 1)
            #expect(vacuum.last?.error != nil, "VACUUM INTO, writes \(writes)")
            #expect(!FileManager.default.fileExists(atPath: out))
            let attach = try await d.run("ATTACH DATABASE '\(other)' AS o; SELECT * FROM o.secret", cap: 1)
            #expect(attach.count == 1 && attach[0].error?.contains("too many attached databases") == true, "ATTACH, writes \(writes)")
            let fresh = (dir as NSString).appendingPathComponent("fresh-\(writes).db")
            let create = try await d.run("ATTACH DATABASE '\(fresh)' AS f; CREATE TABLE f.x (y)", cap: 1)
            #expect(create.first?.error != nil)
            #expect(!FileManager.default.fileExists(atPath: fresh))
            // defensive mode: no writable_schema
            if writes {
                let schema = try await d.run("PRAGMA writable_schema = ON; UPDATE sqlite_schema SET sql = 'x' WHERE name = 't'", cap: 1)
                #expect(schema.last?.error != nil)
            }
        }
        let still = try await SQLiteDriver(path: path, allowWrites: false).run("SELECT count(*) FROM t", cap: 1)
        #expect(still.last?.outcome == .rows(SQLResultSet(columns: [SQLColumn("count(*)")], rows: [["3"]])))
    }

    @Test func allowWritesPermitsThem() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let rw = SQLiteDriver(path: path, allowWrites: true)
        let r = try await rw.run("INSERT INTO t (n) VALUES (99), (100)", cap: 10)
        #expect(r == [SQLStatementResult(sql: "INSERT INTO t (n) VALUES (99), (100)", outcome: .done(rowsAffected: 2))])
        let c = try await SQLiteDriver(path: path, allowWrites: false).run("SELECT count(*) FROM t", cap: 10)
        #expect(c.last?.outcome == .rows(SQLResultSet(columns: [SQLColumn("count(*)")], rows: [["5"]])))
    }

    @Test func valuesTypesAndNulls() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let r = try await SQLiteDriver(path: path, allowWrites: false).run("SELECT n, s, b, r, n * 2 AS twice FROM t ORDER BY n", cap: 10)
        guard case .rows(let set) = r.last?.outcome else { Issue.record("no rows"); return }
        #expect(set.columns == [SQLColumn("n", type: "INTEGER"), SQLColumn("s", type: "TEXT"), SQLColumn("b", type: "BLOB"), SQLColumn("r", type: "REAL"), SQLColumn("twice")])
        #expect(set.rows == [
            ["1", "row 1", nil, "0.5", "2"],
            ["2", nil, "x'00ff10'", "1.0", "4"],
            ["3", "row 3", nil, "1.5", "6"],
        ])
    }

    @Test func capsAt1000AndCountsTheRest() async throws {
        let path = try await Self.makeDB(rows: 1500)
        defer { Self.cleanup(path) }
        let outcome = await SQLRunner.run(SQLiteDriver(path: path, allowWrites: false), sql: "SELECT * FROM t")
        let set = try #require(outcome.lastResultSet)
        #expect(set.rows.count == 1000)
        #expect(set.totalRows == 1500)
        #expect(set.droppedRows == 500)
        #expect(outcome.succeeded)
    }

    @Test func multipleStatementsRunInOrderAndStopAtAnError() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let d = SQLiteDriver(path: path, allowWrites: false)
        let r = try await d.run("SELECT 1 AS a;\n-- a comment\nSELECT 2 AS b;", cap: 10)
        #expect(r.map(\.sql) == ["SELECT 1 AS a;", "SELECT 2 AS b;"])
        #expect(r.last?.outcome == .rows(SQLResultSet(columns: [SQLColumn("b")], rows: [["2"]])))
        let bad = try await d.run("SELECT 1; SELEC 2; SELECT 3", cap: 10)
        #expect(bad.count == 2)
        #expect(bad[1].error?.contains("syntax error") == true)
        #expect(bad[1].sql == "SELEC 2")
        let missing = try await d.run("SELECT * FROM nope", cap: 10)
        #expect(missing.last?.error?.contains("no such table") == true)
        #expect(try await d.run("-- nothing", cap: 10) == [])
    }

    @Test func missingFileSaysSoAndIsNeverCreated() async throws {
        let path = FileManager.default.temporaryDirectory.appendingPathComponent("taisce-nope-\(UUID().uuidString).db").path
        for writes in [false, true] {
            let outcome = await SQLRunner.run(SQLiteDriver(path: path, allowWrites: writes), sql: "SELECT 1")
            #expect(outcome.message?.contains("No database file") == true)
            #expect(!FileManager.default.fileExists(atPath: path))
        }
    }

    @Test func notADatabase() async throws {
        let path = FileManager.default.temporaryDirectory.appendingPathComponent("taisce-text-\(UUID().uuidString).db").path
        try Data(String(repeating: "hello, not sqlite\n", count: 100).utf8).write(to: URL(fileURLWithPath: path))
        defer { try? FileManager.default.removeItem(atPath: path) }
        await #expect(throws: SQLDriverError.self) { try await SQLiteDriver(path: path, allowWrites: false).testConnection() }
    }

    @Test func testConnection() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let line = try await SQLiteDriver(path: path, allowWrites: false).testConnection()
        #expect(line.hasPrefix("SQLite 3."))
        #expect(line.hasSuffix("read-only"))
    }

    static let forever = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c"

    @Test func stopInterruptsARunawayQuery() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let d = SQLiteDriver(path: path, allowWrites: false)
        let started = ContinuousClock.now
        let task = Task { await SQLRunner.run(d, sql: Self.forever) }
        try await Task.sleep(for: .milliseconds(300))
        task.cancel()
        let outcome = await task.value
        #expect(outcome.stopped)
        #expect(!outcome.timedOut)
        #expect(ContinuousClock.now - started < .seconds(5))
    }

    @Test func theTimeoutInterruptsToo() async throws {
        let path = try await Self.makeDB()
        defer { Self.cleanup(path) }
        let outcome = await SQLRunner.run(SQLiteDriver(path: path, allowWrites: false), sql: Self.forever, timeout: .milliseconds(300))
        #expect(outcome.timedOut)
        #expect(!outcome.stopped)
        #expect(outcome.duration < .seconds(5))
    }

    @Test func tildeExpands() {
        #expect(SQLiteDriver(path: "~/a.db", allowWrites: false).expandedPath == NSHomeDirectory() + "/a.db")
    }
}

// MARK: ClickHouse (requests and parsing; the server itself is below)

@Suite struct ClickHouseTests {
    let driver = ClickHouseDriver(url: URL(string: "https://ch.example:8443")!, database: "analytics", user: "me", password: "s3cret", allowWrites: false)

    @Test func requestCarriesReadOnlyAndNoSecretsInTheURL() throws {
        let r = driver.request("SELECT 1", queryID: "q1")
        let url = try #require(r.url?.absoluteString)
        #expect(url == "https://ch.example:8443/?readonly=1&query_id=q1&default_format=JSONCompactEachRowWithNamesAndTypes&database=analytics")
        #expect(!url.contains("s3cret"))
        #expect(r.httpMethod == "POST")
        #expect(r.httpBody == Data("SELECT 1".utf8))
        #expect(r.value(forHTTPHeaderField: "X-ClickHouse-User") == "me")
        #expect(r.value(forHTTPHeaderField: "X-ClickHouse-Key") == "s3cret")

        var rw = driver
        rw.allowWrites = true
        rw.database = ""
        rw.password = nil
        let w = rw.request("INSERT", queryID: "q2")
        #expect(w.url?.absoluteString == "https://ch.example:8443/?query_id=q2&default_format=JSONCompactEachRowWithNamesAndTypes")
        #expect(w.value(forHTTPHeaderField: "X-ClickHouse-Key") == nil)
    }

    @Test func killQuery() throws {
        let k = driver.killRequest(queryID: "a'b")
        #expect(k.httpBody == Data("KILL QUERY WHERE query_id = 'a\\'b' ASYNC".utf8))
        #expect(k.url?.query()?.contains("readonly") == false)
    }

    @Test func elements() {
        #expect(ClickHouseResultParser.elements(#"[1, 5, null, [1,2], 1.5, "2026-10-03 07:54:17"]"#) == ["1", "5", nil, "[1,2]", "1.5", "2026-10-03 07:54:17"])
        #expect(ClickHouseResultParser.elements(#"["a, b", "q\"uote", {"k":[1,"]"]}, true]"#) == ["a, b", "q\"uote", #"{"k":[1,"]"]}"#, "true"])
        #expect(ClickHouseResultParser.elements("[]") == [])
        #expect(ClickHouseResultParser.elements(#"["é\n"]"#) == ["é\n"])
        #expect(ClickHouseResultParser.elements("Code: 62. DB::Exception") == nil)
        #expect(ClickHouseResultParser.elements(#"["unterminated]"#) == nil)
    }

    @Test func parsesNamesTypesAndRowsWithACap() {
        var p = ClickHouseResultParser(cap: 2, format: ClickHouseDriver.format)
        for l in [#"["a", "b"]"#, #"["UInt8", "Nullable(String)"]"#, #"[1, "x"]"#, #"[2, null]"#, #"[3, "z"]"#] { p.add(l) }
        #expect(p.outcome(summary: nil) == .rows(SQLResultSet(
            columns: [SQLColumn("a", type: "UInt8"), SQLColumn("b", type: "Nullable(String)")],
            rows: [["1", "x"], ["2", nil]], totalRows: 3
        )))
    }

    @Test func emptyBodyIsAStatementWithoutRows() {
        let p = ClickHouseResultParser(cap: 10, format: nil)
        #expect(p.outcome(summary: #"{"written_rows":"7"}"#) == .done(rowsAffected: 7))
        #expect(p.outcome(summary: #"{"written_rows":"0"}"#) == .done(rowsAffected: nil))
        // a SELECT with no rows still has its names and types
        var q = ClickHouseResultParser(cap: 10, format: nil)
        q.add(#"["a"]"#)
        q.add(#"["UInt8"]"#)
        #expect(q.outcome(summary: nil) == .rows(SQLResultSet(columns: [SQLColumn("a", type: "UInt8")])))
    }

    @Test func anExceptionMidStreamIsAnError() {
        var p = ClickHouseResultParser(cap: 10, format: nil)
        for l in [#"["a"]"#, #"["UInt8"]"#, "[1]", "__exception__", "Code: 395. DB::Exception: boom. (FUNCTION_THROW_IF_VALUE_IS_NON_ZERO)", "[2]"] { p.add(l) }
        guard case .failed(let e) = p.outcome(summary: nil) else { Issue.record("not failed"); return }
        #expect(e.hasPrefix("Code: 395."))
        #expect(e.contains("[2]"))
    }

    @Test func anotherFormatIsShownAsText() {
        var p = ClickHouseResultParser(cap: 10, format: "CSV")
        p.add("1,\"a\"")
        p.add("2,\"b\"")
        #expect(p.outcome(summary: nil) == .rows(SQLResultSet(columns: [SQLColumn("output (CSV)")], rows: [["1,\"a\""], ["2,\"b\""]])))
    }

    @Test func fromASource() throws {
        var s = DataSource(name: "ch", kind: .clickhouse)
        s.url = "http://localhost:8123"
        s.database = "db"
        let d = try ClickHouseDriver(s, password: "pw")
        #expect(d.url.absoluteString == "http://localhost:8123")
        #expect(d.user == "default")
        #expect(!d.allowWrites)
        s.url = "not a url"
        #expect(throws: SQLDriverError.self) { try ClickHouseDriver(s, password: nil) }
    }
}
