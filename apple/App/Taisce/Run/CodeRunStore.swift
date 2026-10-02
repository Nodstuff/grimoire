import Foundation
import Observation
import SwiftUI
import TaisceKit

/// Where a code block sits: its doc and block, and whether you may save a
/// practice edit back to it. Set per block by the doc's reading view.
struct CodeRunContext: Hashable {
    var doc: DocID
    var block: BlockID
    var canSave: Bool
}

/// One code block's run state for this session: the try line, the
/// practice text, and the last run's output. Kept by `CodeRunStore` so it
/// survives the block scrolling away; nothing is persisted.
@MainActor @Observable
final class BlockRunState: Identifiable {
    nonisolated let id: String
    var tryLine = ""
    /// "Edit to try": the text being tried (nil = the doc's code)
    var practice: String?
    /// the doc's code when the practice text was last in step with it: an
    /// untouched practice text is the doc's, with the doc's trust
    private(set) var practiceBase: String?
    /// Run pressed: who wrote the code is being checked
    var isChecking = false
    var isRunning = false
    var phase: String?
    var log = OutputLog()
    var result: RunResult?
    var startedAt: Date?
    /// the run is waiting on "Last edited by …. Run it on this Mac?"
    var trustPrompt: TrustPrompt?
    var saveError: String?
    var isSaving = false
    #if targetEnvironment(macCatalyst)
    @ObservationIgnored var current: CodeRun?
    #endif
    /// Stop the run in flight, if any (Mac).
    func stopCurrent() {
        #if targetEnvironment(macCatalyst)
        current?.stop()
        #endif
    }

    init(id: String) {
        self.id = id
    }

    var hasOutput: Bool { isChecking || isRunning || result != nil || !log.isEmpty }
    var isBusy: Bool { isChecking || isRunning }

    /// The code a Run uses: the practice text, else the doc's.
    func code(doc: String) -> String { practice ?? doc }

    /// You typed in the practice text (it differs from where it started).
    var isPracticeEdited: Bool { practice != nil && practice != practiceBase }

    func beginPractice(_ docCode: String) {
        if practice == nil {
            practice = docCode
            practiceBase = docCode
        }
    }

    func endPractice() {
        practice = nil
        practiceBase = nil
    }

    /// The doc's code changed under an untouched practice text: follow it,
    /// so a Run never executes an older version nobody approved.
    func followDoc(_ docCode: String) {
        guard practice != nil, !isPracticeEdited, practiceBase != docCode else { return }
        practice = docCode
        practiceBase = docCode
    }

    /// A run couldn't start: say why where its output would be.
    func failBeforeStart(_ message: String) {
        isChecking = false
        isRunning = false
        phase = nil
        log = OutputLog()
        result = RunResult(message: message)
    }
}

struct TrustPrompt: Identifiable, Hashable {
    let id = UUID()
    var lastEditedBy: String
    var code: String
    var approval: RunTrust.Approval
    var request: RunRequest
    var doc: DocID
}

/// The runs of this session, per block, and the Run / Stop / trust / save
/// flow. Mac only in practice (the view offers Run only there).
@MainActor @Observable
final class CodeRunStore {
    @ObservationIgnored private var states: [String: BlockRunState] = [:]
    @ObservationIgnored weak var app: AppModel?
    @ObservationIgnored private var me: RunTrust.Me?
    @ObservationIgnored private var meServer: String?
    /// how long "Checking who wrote this…" may take before it asks anyway
    @ObservationIgnored var trustCheckLimit: Duration = .seconds(5)

    func state(_ c: CodeRunContext) -> BlockRunState {
        let key = "\(c.doc)/\(c.block)"
        if let s = states[key] { return s }
        let s = BlockRunState(id: key)
        states[key] = s
        return s
    }

    /// A new server or person: nothing carries over.
    func reset() {
        for s in states.values { s.stopCurrent() }
        states = [:]
        me = nil
        meServer = nil
    }

    // MARK: running

    /// Run, or ask first when someone else last wrote the code. Busy
    /// (checking or running) is the only no-op: the card shows a spinner or
    /// Stop then. A prompt left over from before is replaced.
    func run(_ s: BlockRunState, _ request: RunRequest, context: CodeRunContext, docCode: String) async {
        guard !s.isBusy else { return }
        s.trustPrompt = nil
        s.followDoc(docCode)
        var request = request
        request.code = s.code(doc: docCode)
        s.isChecking = true
        s.result = nil
        s.log = OutputLog()
        s.phase = "Checking who wrote this\u{2026}"
        s.startedAt = .now
        let (decision, approval) = await trust(context, practiceEdited: s.isPracticeEdited)
        s.isChecking = false
        s.phase = nil
        switch decision {
        case .run:
            await start(s, request)
        case .ask(let name):
            s.trustPrompt = TrustPrompt(lastEditedBy: name, code: request.code, approval: approval, request: request, doc: context.doc)
        }
    }

    /// "Run" on the question, with the prompt the sheet showed (not
    /// whatever `trustPrompt` holds by then: dismissing the sheet clears
    /// it). Remembered for this doc on this device.
    func confirm(_ s: BlockRunState, _ p: TrustPrompt) async {
        if s.trustPrompt?.id == p.id { s.trustPrompt = nil }
        guard !s.isBusy else { return }
        approvals?.approve(p.doc, p.approval)
        await start(s, p.request)
    }

    func cancelPrompt(_ s: BlockRunState) {
        s.trustPrompt = nil
    }

    func stop(_ s: BlockRunState) {
        s.stopCurrent()
    }

    private func start(_ s: BlockRunState, _ request: RunRequest) async {
        #if targetEnvironment(macCatalyst)
        s.isRunning = true
        s.log = OutputLog()
        s.result = nil
        s.phase = "Starting\u{2026}"
        s.startedAt = .now
        let env = await LoginEnvironmentCache.shared.environment()
        if let note = await LoginEnvironmentCache.shared.note() {
            s.log.append([OutputChunk(.stderr, "(\(note))\n")])
        }
        let run = CodeRun(request, environment: env)
        s.current = run
        // at most one event per ~50 ms tick: the UI keeps up with anything
        for await e in run.events {
            switch e {
            case .phase(let p): s.phase = p
            case .output(let chunks): s.log.append(chunks)
            case .truncated: s.log.truncated = true
            case .finished(let r): s.result = r
            }
        }
        s.current = nil
        s.phase = nil
        s.isRunning = false
        #else
        s.failBeforeStart("Code runs on the Mac.")
        #endif
    }

    // MARK: trust

    private var approvals: RunApprovals? {
        app.map { RunApprovals(server: $0.serverURL) }
    }

    /// Who wrote it, bounded by `trustCheckLimit`: a slow or failing check
    /// counts as unknown (so it asks), and the prompt says why.
    private func trust(_ c: CodeRunContext, practiceEdited: Bool) async -> (RunTrust.Decision, RunTrust.Approval) {
        let docEpoch = (try? await app?.cache?.doc(c.doc))?.currentEpoch ?? 0
        var history: [DocHistoryEntry]?
        var why: String?
        if let api = app?.api {
            let id = c.doc
            switch await withTimeLimit(trustCheckLimit, { try await api.docHistory(id) }) {
            case .finished(let h): history = h
            case .threw(let e): why = e
            case .timedOut: why = "the server didn't answer in \(RunStatus.seconds(trustCheckLimit))"
            }
        } else {
            why = "not connected"
        }
        let me = await currentMe()
        var decision = RunTrust.decide(
            block: c.block, history: history, me: me, practiceEditedByMe: practiceEdited,
            approval: approvals?.approval(for: c.doc), docEpoch: docEpoch
        )
        if case .ask = decision, history == nil, let why {
            decision = .ask(lastEditedBy: "someone (couldn't check who: \(why))")
        }
        return (decision, RunTrust.approval(history: history, me: me, docEpoch: docEpoch))
    }

    /// The signed-in person (`/api/profile`), once per server; bounded too.
    private func currentMe() async -> RunTrust.Me {
        if let me, meServer == app?.serverURL { return me }
        guard let app, let api = app.api,
              case .finished(let p) = await withTimeLimit(trustCheckLimit, { try await api.profile() })
        else {
            return RunTrust.Me(principalID: nil, name: nil)
        }
        let m = RunTrust.Me(principalID: p.principalID?.lowercased(), name: p.name)
        me = m
        meServer = app.serverURL
        return m
    }

    // MARK: practice edits

    func beginPractice(_ s: BlockRunState, docCode: String) {
        s.beginPractice(docCode)
        s.saveError = nil
    }

    func revert(_ s: BlockRunState) {
        s.endPractice()
        s.saveError = nil
    }

    /// "Save to doc": the block's fence body replaced, through the outbox
    /// like any editor save (so the review gate applies).
    func save(_ s: BlockRunState, context c: CodeRunContext) async {
        guard let text = s.practice, let app, !s.isSaving else { return }
        s.isSaving = true
        defer { s.isSaving = false }
        do {
            try await app.replaceBlockText(doc: c.doc, block: c.block) { FenceEdit.replacingCode(in: $0, with: text) }
            s.endPractice()
            s.saveError = nil
        } catch {
            s.saveError = error.localizedDescription
        }
    }
}

/// The trust sheet's wiring, outside the view so tests drive exactly what
/// the card does.
@MainActor
enum TrustFlow {
    /// The sheet's item: dismissing (Cancel, Esc, a swipe) clears the
    /// prompt. Confirming doesn't depend on it (`confirm` has the prompt).
    static func presentation(_ store: CodeRunStore, _ s: BlockRunState) -> Binding<TrustPrompt?> {
        Binding(get: { s.trustPrompt }, set: { if $0 == nil { store.cancelPrompt(s) } })
    }

    /// What the sheet's Run button does: start the run with this prompt,
    /// then close the sheet.
    @discardableResult
    static func runPressed(_ store: CodeRunStore, _ s: BlockRunState, _ p: TrustPrompt, dismiss: () -> Void) -> Task<Void, Never> {
        let task = Task { await store.confirm(s, p) }
        dismiss()
        return task
    }

    static func cancelPressed(_ store: CodeRunStore, _ s: BlockRunState, dismiss: () -> Void) {
        store.cancelPrompt(s)
        dismiss()
    }
}
