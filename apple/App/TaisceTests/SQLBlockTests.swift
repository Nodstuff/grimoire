import Foundation
import SwiftUI
import Testing
import TaisceKit
@testable import Taisce

/// SQL blocks in the app: data sources (file + Keychain), which source a
/// block runs on, the status line, and (Mac) real SQLite runs through the
/// trust flow, Save db= and the card's layout.
@MainActor @Suite(.serialized) struct SQLBlockTests {
    func tempStore() -> (DataSourceStore, URL) {
        let dir = FileManager.default.temporaryDirectory.appending(path: "taisce-ds-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
        let secrets = MemoryDataSourceSecrets()
        return (DataSourceStore(file: DataSourceFile(url: dir.appending(path: "datasources.json")), secrets: { secrets }), dir)
    }

    @Test func aTestHostUsesThrowawayPlaces() throws {
        #expect(AppPaths.isTestHost)
        let file = try #require(AppPaths.dataSourcesFile)
        #expect(file.path.hasPrefix(FileManager.default.temporaryDirectory.path))
        #expect(AppPaths.dataSourceKeychainIdentifier.hasPrefix("ie.null.taisce.tests.datasource."))
        #expect(AppPaths.dataSourceKeychainIdentifier != KeychainDataSourceSecrets.defaultIdentifier)
    }

    @Test func storeSavesValidatesAndDeletes() throws {
        let (store, dir) = tempStore()
        defer { try? FileManager.default.removeItem(at: dir) }
        #expect(store.sources.isEmpty)
        var pg = DataSource(name: "warehouse", kind: .postgres)
        #expect(throws: SQLDriverError.self) { try store.save(pg, password: "pw") }
        pg.host = "db.example"
        pg.user = "me"
        try store.save(pg, password: "pw")
        #expect(store.sources.map(\.name) == ["warehouse"])
        #expect(store.hasPassword(pg))
        // nil keeps the password; the name must stay unique
        pg.port = 6543
        try store.save(pg, password: nil)
        #expect(store.hasPassword(pg))
        #expect(store.sources.first?.port == 6543)
        var clash = DataSource(name: "Warehouse", kind: .sqlite)
        clash.path = "/tmp/x.db"
        #expect(throws: SQLDriverError.self) { try store.save(clash, password: nil) }
        #expect(store.source(named: "WAREHOUSE")?.id == pg.id)
        // a fresh store reads the same file (passwords aren't in it)
        let again = DataSourceStore(file: store.file, secrets: { MemoryDataSourceSecrets() })
        #expect(again.sources == store.sources)
        let json = try String(contentsOf: try #require(store.file?.url), encoding: .utf8)
        #expect(!json.contains("\"pw\""))
        try store.delete(pg)
        #expect(store.sources.isEmpty)
        #expect(!store.hasPassword(pg))
    }

    /// A password the Keychain won't take: the list goes back to what it
    /// was, nothing is half-saved, and the message says so.
    @Test func aPasswordThatCantBeStoredSavesNothing() throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "taisce-ds-tests-\(UUID().uuidString)", directoryHint: .isDirectory)
        defer { try? FileManager.default.removeItem(at: dir) }
        let file = DataSourceFile(url: dir.appending(path: "datasources.json"))
        let secrets = FailingSecrets()
        let store = DataSourceStore(file: file, secrets: { secrets })
        var kept = DataSource(name: "kept", kind: .sqlite)
        kept.path = "/tmp/k.db"
        try store.save(kept, password: nil)
        var pg = DataSource(name: "warehouse", kind: .postgres)
        pg.host = "db.example"
        pg.user = "me"
        #expect {
            try store.save(pg, password: "pw")
        } throws: { e in
            (e as? SQLDriverError)?.message.contains("wasn't saved") == true
        }
        #expect(store.sources == [kept])
        #expect(try file.load() == [kept])
    }

    /// This launch's test identifier is stamped (so another test host can
    /// tell it from an orphan), and no orphan of an earlier launch is left
    /// in the Keychain: the sweep at launch took them.
    @Test func noOrphanedTestKeychainItems() throws {
        let id = AppPaths.dataSourceKeychainIdentifier
        #expect(!KeychainDataSourceSecrets.isRemovableTestIdentifier(id, current: nil, cutoff: .now.addingTimeInterval(-1800)))
        // an orphan made now, stamped two hours ago, is swept; this
        // launch's own item is not (until the launch ends)
        let orphanID = KeychainDataSourceSecrets.testIdentifier(now: .now.addingTimeInterval(-7200))
        let orphan = try KeychainDataSourceSecrets(identifier: orphanID)
        try orphan.setPassword("old", for: "src")
        let mine = try KeychainDataSourceSecrets(identifier: id)
        try mine.setPassword("mine", for: "src-sweep")
        defer { try? mine.setPassword(nil, for: "src-sweep") }
        #expect(AppPaths.sweepTestKeychain(current: nil) >= 1)
        #expect(try orphan.password(for: "src") == nil)
        #expect(try mine.password(for: "src-sweep") == "mine")
        // and at the end of a launch its own go too
        AppPaths.sweepTestKeychain(current: id)
        #expect(try mine.password(for: "src-sweep") == nil)
    }

    @Test func keychainPasswordsUnderAThrowawayIdentifier() throws {
        let id = KeychainDataSourceSecrets.testIdentifier()
        let k = try KeychainDataSourceSecrets(identifier: id)
        defer { try? k.removeAll() }
        try k.setPassword("s3cret", for: "src-1")
        #expect(try KeychainDataSourceSecrets(identifier: id).password(for: "src-1") == "s3cret")
        try k.setPassword(nil, for: "src-1")
        #expect(try k.password(for: "src-1") == nil)
    }

    @Test func whichSourceABlockRunsOn() {
        var lite = DataSource(name: "local", kind: .sqlite)
        lite.path = "/x.db"
        var pg = DataSource(name: "pg", kind: .postgres)
        pg.host = "h"
        let all = [lite, pg]
        #expect(SQLSourceChoice.resolve(fence: SQLFence("sql")!, db: "LOCAL", sources: all) == .ready(lite))
        #expect(SQLSourceChoice.resolve(fence: SQLFence("sql")!, db: nil, sources: all) == .pick(all))
        #expect(SQLSourceChoice.resolve(fence: SQLFence("postgres")!, db: nil, sources: all) == .pick([pg]))
        #expect(SQLSourceChoice.resolve(fence: SQLFence("sql")!, db: "nope", sources: all) == .unknown("nope"))
        #expect(SQLSourceChoice.resolve(fence: SQLFence("sqlite")!, db: "pg", sources: all) == .wrongKind(pg, wanted: .sqlite))
        #expect(SQLSourceChoice.resolve(fence: SQLFence("clickhouse")!, db: "", sources: []) == .pick([]))
    }

    @Test func statusLines() {
        let rows = SQLStatementResult(sql: "SELECT", outcome: .rows(SQLResultSet(columns: [SQLColumn("a")], rows: Array(repeating: ["1"], count: 1000), isCapped: true)))
        #expect(SQLStatus.line(SQLRunOutcome(statements: [rows], duration: .milliseconds(120))) == ("1,000+ rows · 0.12 s", true))
        #expect(SQLStatus.line(SQLRunOutcome(statements: [rows, rows], duration: .seconds(1))).text == "2 statements · 1,000+ rows · 1.00 s")
        let one = SQLStatementResult(sql: "SELECT", outcome: .rows(SQLResultSet(columns: [SQLColumn("a")], rows: [["1"]])))
        #expect(SQLStatus.line(SQLRunOutcome(statements: [one], duration: .seconds(1))).text == "1 row · 1.00 s")
        #expect(SQLStatus.line(SQLRunOutcome(statements: [SQLStatementResult(sql: "INSERT", outcome: .done(rowsAffected: 1))], duration: .seconds(1))).text == "1 row affected · 1.00 s")
        #expect(SQLStatus.line(SQLRunOutcome(statements: [SQLStatementResult(sql: "CREATE", outcome: .done(rowsAffected: nil))], duration: .seconds(1))).text == "OK · 1.00 s")
        let failed = SQLRunOutcome(statements: [rows, SQLStatementResult(sql: "SELEC", outcome: .failed("syntax"))], duration: .seconds(1))
        #expect(SQLStatus.line(failed) == ("Statement 2 failed · 1.00 s", false))
        #expect(SQLStatus.line(SQLRunOutcome(statements: [SQLStatementResult(sql: "x", outcome: .failed("e"))], duration: .seconds(1))).text == "Failed · 1.00 s")
        #expect(SQLStatus.line(SQLRunOutcome(duration: .seconds(1), stopped: true)).text == "Stopped · 1.00 s")
        #expect(SQLStatus.line(SQLRunOutcome(duration: .seconds(300), timedOut: true)).text == "Timed out after 5 min 0 s (limit 5 min)")
        #expect(SQLStatus.line(SQLRunOutcome(message: "No database file at /x")) == ("No database file at /x", false))
    }

    #if targetEnvironment(macCatalyst)
    @Test func columnWidthsFollowTheContent() {
        let set = SQLResultSet(columns: [SQLColumn("id", type: "INTEGER"), SQLColumn("body")], rows: [["1", String(repeating: "x", count: 200)], ["22", nil]])
        let w = SQLTable.columnWidths(set)
        #expect(w[0] == 7 * SQLTable.charWidth + 16)
        #expect(w[1] == 48 * SQLTable.charWidth + 16)
    }
    #endif

    // MARK: in the app, against a real SQLite file

    /// A model on an unreachable LOCAL server holding a doc with a SQL block.
    func modelWithSQLDoc(_ content: String) async throws -> (AppModel, DocID, BlockID) {
        let m = AppModel()
        m.discover = { _ in nil }
        await m.setServerURL("http://127.0.0.1:9")
        if m.cache == nil { await m.boot() }
        await m.stopSync()
        UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey)
        let cache = try #require(m.cache)
        let doc = "sql-\(UUID().uuidString.prefix(8))".lowercased()
        let block = "\(doc)-b"
        try await cache.storeDoc(DocTree(
            doc: DocSummary(id: doc, parentID: nil, title: "Queries", currentEpoch: 2),
            roots: [BlockNode(block: Block(id: block, docID: doc, parentID: nil, orderKey: "a", blockType: .code, content: content, epoch: 2))]
        ))
        return (m, doc, block)
    }

    /// A SQLite file with `t(n, s)`: 1..rows, s NULL on even n.
    func sqliteFile(rows: Int = 3) async throws -> String {
        let dir = FileManager.default.temporaryDirectory.appending(path: "taisce-sql-app-\(UUID().uuidString)", directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let path = dir.appending(path: "q.db").path
        FileManager.default.createFile(atPath: path, contents: nil)
        let r = try await SQLiteDriver(path: path, allowWrites: true).run("""
        CREATE TABLE t (n INTEGER, s TEXT);
        WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < \(rows))
        INSERT INTO t SELECT x, CASE WHEN x % 2 = 0 THEN NULL ELSE 'row ' || x END FROM c;
        """, cap: 1)
        #expect(r.allSatisfy { $0.error == nil })
        return path
    }

    func addSource(_ m: AppModel, path: String, writes: Bool = false) throws -> DataSource {
        var s = DataSource(name: "t\(UUID().uuidString.prefix(6).lowercased())", kind: .sqlite)
        s.path = path
        s.allowWrites = writes
        try m.dataSources.save(s, password: nil)
        return s
    }

    #if targetEnvironment(macCatalyst)
    @Test func runAsksOfflineThenShowsTheRowsAndRemembers() async throws {
        let path = try await sqliteFile(rows: 1200)
        let (m, doc, block) = try await modelWithSQLDoc("```sql\nSELECT * FROM t\n```")
        let src = try addSource(m, path: path)
        defer { try? m.dataSources.delete(src) }
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.sqlRuns.state(ctx)
        let e = m.codeRuns.state(ctx)
        // offline: who wrote it is unknown → ask
        await m.sqlRuns.run(s, edit: e, context: ctx, docCode: "SELECT * FROM t ORDER BY n", source: src, picked: true)
        let p = try #require(s.prompt)
        #expect(p.lastEditedBy?.hasPrefix("someone (couldn't check who: ") == true)
        #expect(!p.writes)
        let binding = SQLTrustFlow.presentation(m.sqlRuns, s)
        await SQLTrustFlow.runPressed(m.sqlRuns, s, p) { binding.wrappedValue = nil }.value
        #expect(s.prompt == nil)
        let set = try #require(s.outcome?.lastResultSet)
        #expect(set.columns == [SQLColumn("n", type: "INTEGER"), SQLColumn("s", type: "TEXT")])
        #expect(set.rows.count == 1000 && set.isCapped)
        #expect(set.rows[0] == ["1", "row 1"] && set.rows[1] == ["2", nil])
        #expect(s.sourceName == src.name && s.pickedSource)
        // approved: the next run goes straight through
        await m.sqlRuns.run(s, edit: e, context: ctx, docCode: "SELECT count(*) FROM t", source: src, picked: false)
        #expect(s.prompt == nil)
        // ...but a go/shell block in the same doc still asks
        let (code, _) = await m.codeRuns.trust(ctx, practiceEdited: false)
        #expect(code != .run)
        #expect(m.codeRuns.approvals?.approval(for: doc) == nil)
        #expect(s.outcome?.lastResultSet?.rows == [["1200"]])
        // read-only: the database refuses a write
        m.codeRuns.beginPractice(e, docCode: "SELECT 1")
        e.practice = "DELETE FROM t"
        await m.sqlRuns.run(s, edit: e, context: ctx, docCode: "SELECT 1", source: src, picked: false)
        #expect(s.outcome?.failure?.error.contains("readonly") == true)
        RunApprovals(server: m.serverURL, kind: .sql).forget()
        try await m.cache?.deleteDoc(doc)
    }

    @Test func aSourceThatAllowsWritesAlwaysAsks() async throws {
        let path = try await sqliteFile()
        let (m, doc, block) = try await modelWithSQLDoc("```sql\nINSERT INTO t VALUES (9, 'x')\n```")
        let src = try addSource(m, path: path, writes: true)
        defer { try? m.dataSources.delete(src) }
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.sqlRuns.state(ctx)
        let e = m.codeRuns.state(ctx)
        // your own practice text: no who-wrote-it question, but writes still ask
        m.codeRuns.beginPractice(e, docCode: "SELECT 1")
        e.practice = "INSERT INTO t VALUES (9, 'x')"
        for round in 1...2 {
            await m.sqlRuns.run(s, edit: e, context: ctx, docCode: "SELECT 1", source: src, picked: false)
            let p = try #require(s.prompt, "round \(round)")
            #expect(p.writes && p.lastEditedBy == nil)
            await m.sqlRuns.confirm(s, p)
            #expect(s.outcome?.statements.last?.outcome == .done(rowsAffected: 1))
        }
        // Cancel runs nothing
        await m.sqlRuns.run(s, edit: e, context: ctx, docCode: "SELECT 1", source: src, picked: false)
        #expect(s.prompt != nil)
        m.sqlRuns.cancelPrompt(s)
        #expect(s.prompt == nil && !s.isBusy)
        let count = try await SQLiteDriver(path: path, allowWrites: false).run("SELECT count(*) FROM t WHERE n = 9", cap: 1)
        #expect(count.last?.outcome == .rows(SQLResultSet(columns: [SQLColumn("count(*)")], rows: [["2"]])))
        RunApprovals(server: m.serverURL, kind: .sql).forget()
        try await m.cache?.deleteDoc(doc)
    }

    @Test func stopEndsARunawayQuery() async throws {
        let path = try await sqliteFile()
        let (m, doc, block) = try await modelWithSQLDoc("```sql\nSELECT 1\n```")
        let src = try addSource(m, path: path)
        defer { try? m.dataSources.delete(src) }
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.sqlRuns.state(ctx)
        let e = m.codeRuns.state(ctx)
        m.codeRuns.beginPractice(e, docCode: "SELECT 1")
        e.practice = "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c) SELECT count(*) FROM c"
        let running = Task { await m.sqlRuns.run(s, edit: e, context: ctx, docCode: "SELECT 1", source: src, picked: false) }
        try await Task.sleep(for: .milliseconds(400))
        #expect(s.isRunning)
        m.sqlRuns.stop(s)
        await running.value
        #expect(s.outcome?.stopped == true && !s.isRunning)
        // and the 5 min limit, shortened
        m.sqlRuns.timeout = .milliseconds(300)
        await m.sqlRuns.run(s, edit: e, context: ctx, docCode: "SELECT 1", source: src, picked: false)
        #expect(s.outcome?.timedOut == true)
        try await m.cache?.deleteDoc(doc)
    }

    @Test func savingThePickedSourceIntoTheFence() async throws {
        let (m, doc, block) = try await modelWithSQLDoc("Count:\n```sql\nSELECT 1\n```")
        let cache = try #require(m.cache)
        let before = try await cache.pendingOutbox().count
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.sqlRuns.state(ctx)
        s.sourceName = "warehouse"
        s.pickedSource = true
        await m.sqlRuns.saveSource(s, context: ctx)
        #expect(s.saveError == nil && !s.pickedSource)
        let queued = try await cache.pendingOutbox()
        #expect(queued.count == before + 1)
        let body = try #require(queued.last?.body)
        #expect(String(decoding: body, as: UTF8.self).contains(#"Count:\n```sql db=warehouse\nSELECT 1\n```"#))
        try await cache.deleteDoc(doc)
        _ = try await cache.dropOutbox(forDocs: [doc])
    }

    @Test func theCardLaysOutInEveryState() async throws {
        let path = try await sqliteFile()
        let (m, doc, block) = try await modelWithSQLDoc("```sql\nSELECT 1\n```")
        let src = try addSource(m, path: path)
        defer { try? m.dataSources.delete(src) }
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.sqlRuns.state(ctx)
        s.outcome = SQLRunOutcome(statements: [SQLStatementResult(sql: "SELECT", outcome: .rows(SQLResultSet(
            columns: [SQLColumn("n", type: "INTEGER"), SQLColumn("s")], rows: [["1", "a"], ["2", nil]], isCapped: true
        )))], duration: .milliseconds(30))
        s.sourceName = src.name
        s.pickedSource = true
        try AppModelTests().render(
            VStack {
                RunnableCodeBlock(language: "sql", code: "SELECT 1", attributes: ["db": src.name])
                RunnableCodeBlock(language: "sql", code: "SELECT 1")
                RunnableCodeBlock(language: "postgres", code: "SELECT 1", attributes: ["db": src.name])
                RunnableCodeBlock(language: "clickhouse", code: "SELECT 1", attributes: ["db": "nowhere"])
            }
            .environment(m)
            .environment(\.codeRunContext, ctx)
        )
        s.outcome = SQLRunOutcome(statements: [SQLStatementResult(sql: "SELEC 1", outcome: .failed("near \"SELEC\": syntax error"))])
        try AppModelTests().render(RunnableCodeBlock(language: "sql", code: "SELEC 1", attributes: ["db": src.name]).environment(m).environment(\.codeRunContext, ctx))
        try AppModelTests().render(NavigationStack { DataSourcesScreen() }.environment(m))
        try AppModelTests().render(NavigationStack { DataSourceEditor(original: src, isNew: false) }.environment(m))
        // a weaker connection says what it gives up
        var plain = DataSource(name: "plain", kind: .postgres)
        plain.host = "db.example"
        plain.tlsMode = .disable
        #expect(plain.transportWarning != nil)
        try AppModelTests().render(NavigationStack { DataSourceEditor(original: plain, isNew: true) }.environment(m))
        try await m.cache?.deleteDoc(doc)
    }

    /// The SQL card's "written by …" reads the go/shell card's cached ledger
    /// (CodeRunStore), never a fetch per render; nothing for your own writes.
    @Test func sqlWrittenByComesFromTheCachedLedger() async throws {
        let (m, doc, block) = try await modelWithSQLDoc("```sql\nSELECT 1\n```")
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let me = RunTrust.Me(principalID: "p-tom", name: "Tom", privateWorkspace: true)
        let card = { RunnableCodeBlock(language: "sql", code: "SELECT 1", attributes: ["db": "x"]).environment(m).environment(\.codeRunContext, ctx) }
        #expect(m.codeRuns.writtenBy(ctx) == nil, "nothing cached yet")
        // an unreachable server: nothing cached, nothing shown
        m.codeRuns.trustCheckLimit = .milliseconds(300)
        await m.codeRuns.loadAuthors(ctx)
        #expect(m.codeRuns.histories[doc] == nil)
        try AppModelTests().render(card())
        let agents = DocHistoryEntry(opID: "o2", principalName: "claude:grimoire-sql", principalKind: "agent", principalID: "p-agent", epoch: 2, targetBlock: block, opType: "insert", content: "```sql\nSELECT 1\n```", principalIsYours: true)
        m.codeRuns.remember(doc, epoch: 2, history: [agents], me: me)
        #expect(m.codeRuns.writtenBy(ctx) == "claude:grimoire-sql")
        // cached at the doc's epoch: no refetch
        await m.codeRuns.loadAuthors(ctx)
        #expect(m.codeRuns.histories[doc]?.history.count == 1)
        try AppModelTests().render(card())
        // while trying an edit the label gives way to Revert / Save
        let e = m.codeRuns.state(ctx)
        m.codeRuns.beginPractice(e, docCode: "SELECT 1")
        try AppModelTests().render(card())
        m.codeRuns.revert(e)
        let mine = DocHistoryEntry(opID: "o3", principalName: "Tom", principalKind: "human", principalID: "p-tom", epoch: 3, targetBlock: block, opType: "replace", content: "```sql\nSELECT 2\n```")
        m.codeRuns.remember(doc, epoch: 3, history: [mine, agents], me: me)
        #expect(m.codeRuns.writtenBy(ctx) == nil)
        m.codeRuns.reset()
        try await m.cache?.deleteDoc(doc)
    }
    #else
    @Test func theIPhoneShowsAPlainCodeCard() async throws {
        let (m, doc, block) = try await modelWithSQLDoc("```sql\nSELECT 1\n```")
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        try AppModelTests().render(RunnableCodeBlock(language: "sql", code: "SELECT 1", attributes: ["db": "x"]).environment(m).environment(\.codeRunContext, ctx))
        #expect(throws: SQLDriverError.self) {
            var pg = DataSource(name: "pg", kind: .postgres)
            pg.host = "h"
            _ = try m.dataSources.driver(for: pg)
        }
        try await m.cache?.deleteDoc(doc)
    }
    #endif
}

/// A Keychain that refuses every password.
final class FailingSecrets: DataSourceSecrets {
    struct Refused: Error, LocalizedError {
        var errorDescription: String? { "the Keychain said no" }
    }

    func password(for id: String) throws -> String? { nil }
    func setPassword(_ password: String?, for id: String) throws { throw Refused() }
}
