import Foundation
import Observation
import SwiftUI
import TaisceKit

/// One SQL block's last run, for this session (nothing persisted). The
/// practice text, the trust check's spinner and Save to doc live in the
/// block's `BlockRunState`, shared with go/shell blocks.
@MainActor @Observable
final class SQLBlockState: Identifiable {
    nonisolated let id: String
    var isChecking = false
    var isRunning = false
    var startedAt: Date?
    var outcome: SQLRunOutcome?
    /// the source the last run used
    var sourceName: String?
    /// chosen from the picker (the fence has no `db=`): offer to save it
    var pickedSource = false
    var prompt: SQLPrompt?
    var saveError: String?
    var isSavingDB = false
    @ObservationIgnored var task: Task<SQLRunOutcome, Never>?

    init(id: String) {
        self.id = id
    }

    var isBusy: Bool { isChecking || isRunning }
    var hasOutput: Bool { isBusy || outcome != nil }
}

/// "Run this query?": someone else wrote it, or the source allows writes
/// (that one asks every time).
struct SQLPrompt: Identifiable, Hashable {
    let id = UUID()
    /// nil: you wrote it (only the writes warning applies)
    var lastEditedBy: String?
    var code: String
    var source: DataSource
    var approval: RunTrust.Approval
    var doc: DocID
    var pickedSource: Bool

    var writes: Bool { source.allowWrites }
}

/// SQL blocks' Run / Stop / trust / save-db flow. Trust is the go/shell
/// rule (`CodeRunStore.trust`), plus a question before every run against a
/// source that allows writes.
@MainActor @Observable
final class SQLRunStore {
    @ObservationIgnored private var states: [String: SQLBlockState] = [:]
    @ObservationIgnored weak var app: AppModel?
    /// tests: the per-run limit (the app's is 5 min)
    @ObservationIgnored var timeout: Duration = SQLRunner.timeout

    func state(_ c: CodeRunContext) -> SQLBlockState {
        let key = "\(c.doc)/\(c.block)"
        if let s = states[key] { return s }
        let s = SQLBlockState(id: key)
        states[key] = s
        return s
    }

    func reset() {
        for s in states.values { s.task?.cancel() }
        states = [:]
    }

    /// Run `edit`'s code (practice text or the doc's) against `source`, or
    /// ask first.
    func run(_ s: SQLBlockState, edit: BlockRunState, context: CodeRunContext, docCode: String, source: DataSource, picked: Bool) async {
        guard !s.isBusy, let app else { return }
        s.prompt = nil
        s.saveError = nil
        edit.followDoc(docCode)
        let code = edit.code(doc: docCode)
        s.isChecking = true
        s.startedAt = .now
        s.outcome = nil
        let (decision, approval) = await app.codeRuns.trust(context, practiceEdited: edit.isPracticeEdited, kind: .sql)
        s.isChecking = false
        var asker: String?
        if case .ask(let name) = decision { asker = name }
        if asker != nil || source.allowWrites {
            s.prompt = SQLPrompt(lastEditedBy: asker, code: code, source: source, approval: approval, doc: context.doc, pickedSource: picked)
            return
        }
        await start(s, code: code, source: source, picked: picked)
    }

    /// The sheet's Run: remembered for the doc when it was a who-wrote-it
    /// question (a writes question asks again next time).
    func confirm(_ s: SQLBlockState, _ p: SQLPrompt) async {
        if s.prompt?.id == p.id { s.prompt = nil }
        guard !s.isBusy else { return }
        if p.lastEditedBy != nil { app?.codeRuns.approvals(.sql)?.approve(p.doc, p.approval) }
        await start(s, code: p.code, source: p.source, picked: p.pickedSource)
    }

    func cancelPrompt(_ s: SQLBlockState) {
        s.prompt = nil
    }

    func stop(_ s: SQLBlockState) {
        s.task?.cancel()
    }

    private func start(_ s: SQLBlockState, code: String, source: DataSource, picked: Bool) async {
        guard let app else { return }
        s.sourceName = source.name
        s.pickedSource = picked
        s.startedAt = .now
        s.outcome = nil
        let driver: any SQLDriver
        do {
            driver = try app.dataSources.driver(for: source)
        } catch {
            s.outcome = SQLRunOutcome(message: SQLRunner.describe(error))
            return
        }
        s.isRunning = true
        let timeout = timeout
        let task = Task.detached { await SQLRunner.run(driver, sql: code, timeout: timeout) }
        s.task = task
        // Stop cancels the run's own task (detached: the view going away
        // doesn't cancel it, Stop and the timeout do)
        let outcome = await task.value
        s.task = nil
        s.outcome = outcome
        s.isRunning = false
    }

    /// "Save db=<name> to doc": the fence gets the source the picker chose,
    /// through the outbox like any edit.
    func saveSource(_ s: SQLBlockState, context c: CodeRunContext) async {
        guard let name = s.sourceName, let app, !s.isSavingDB else { return }
        s.isSavingDB = true
        defer { s.isSavingDB = false }
        do {
            try await app.replaceBlockText(doc: c.doc, block: c.block) { FenceEdit.settingAttribute("db", name, in: $0) }
            s.pickedSource = false
            s.saveError = nil
        } catch {
            s.saveError = error.localizedDescription
        }
    }
}

/// What a SQL block can run against, for its card. Pure.
enum SQLSourceChoice: Equatable {
    /// `db=` names a source of a kind the fence accepts
    case ready(DataSource)
    /// no `db=`: pick one of these (maybe none)
    case pick([DataSource])
    /// `db=` names nothing on this Mac
    case unknown(String)
    /// `db=` names a source of another kind than the fence's language
    case wrongKind(DataSource, wanted: DataSourceKind)

    static func resolve(fence: SQLFence, db: String?, sources: [DataSource]) -> SQLSourceChoice {
        guard let db, !db.isEmpty else { return .pick(sources.filter(fence.accepts)) }
        guard let s = sources.first(where: { $0.name.caseInsensitiveCompare(db) == .orderedSame }) else { return .unknown(db) }
        if let k = fence.requiredKind, k != s.kind { return .wrongKind(s, wanted: k) }
        return .ready(s)
    }
}

/// The line over a finished SQL run. Pure.
enum SQLStatus {
    static func line(_ o: SQLRunOutcome) -> (text: String, ok: Bool) {
        let t = RunStatus.seconds(o.duration)
        if let m = o.message { return (m, false) }
        if o.stopped { return ("Stopped · \(t)", false) }
        if o.timedOut { return ("Timed out after \(RunStatus.seconds(o.duration)) (limit 5 min)", false) }
        let n = o.statements.count
        if let f = o.failure {
            return (n > 1 || f.index > 1 ? "Statement \(f.index) failed · \(t)" : "Failed · \(t)", false)
        }
        guard let last = o.statements.last else { return ("Nothing to run · \(t)", true) }
        let prefix = n > 1 ? "\(n) statements · " : ""
        switch last.outcome {
        case .rows(let r):
            return ("\(prefix)\(r.rowCountText) · \(t)", true)
        case .done(let affected):
            return ("\(prefix)\(affected.map { "\(rows($0)) affected" } ?? "OK") · \(t)", true)
        case .failed:
            return ("Failed · \(t)", false)
        }
    }

    static func rows(_ n: Int) -> String { n == 1 ? "1 row" : "\(n.formatted()) rows" }
}

/// The sheet's wiring, outside the view so tests drive what the card does.
@MainActor
enum SQLTrustFlow {
    static func presentation(_ store: SQLRunStore, _ s: SQLBlockState) -> Binding<SQLPrompt?> {
        Binding(get: { s.prompt }, set: { if $0 == nil { store.cancelPrompt(s) } })
    }

    @discardableResult
    static func runPressed(_ store: SQLRunStore, _ s: SQLBlockState, _ p: SQLPrompt, dismiss: () -> Void) -> Task<Void, Never> {
        let task = Task { await store.confirm(s, p) }
        dismiss()
        return task
    }
}
