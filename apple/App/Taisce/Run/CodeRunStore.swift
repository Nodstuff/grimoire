import Foundation
import Observation
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

    var hasOutput: Bool { isRunning || result != nil || !log.isEmpty }

    /// The code a Run uses: the practice text, else the doc's.
    func code(doc: String) -> String { practice ?? doc }

    /// The practice text differs from the doc's code (typed here, so yours).
    func isPracticeEdited(doc: String) -> Bool { practice.map { $0 != doc } ?? false }
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

    /// Run, or ask first when someone else last wrote the code.
    func run(_ s: BlockRunState, _ request: RunRequest, context: CodeRunContext, docCode: String) async {
        guard !s.isRunning, s.trustPrompt == nil else { return }
        let (decision, approval) = await trust(context, practiceEdited: s.isPracticeEdited(doc: docCode))
        switch decision {
        case .run:
            await start(s, request)
        case .ask(let name):
            s.trustPrompt = TrustPrompt(lastEditedBy: name, code: request.code, approval: approval, request: request, doc: context.doc)
        }
    }

    /// "Run" on the question: remembered for this doc on this device.
    func confirm(_ s: BlockRunState) async {
        guard let p = s.trustPrompt else { return }
        s.trustPrompt = nil
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
        #endif
    }

    // MARK: trust

    private var approvals: RunApprovals? {
        app.map { RunApprovals(server: $0.serverURL) }
    }

    private func trust(_ c: CodeRunContext, practiceEdited: Bool) async -> (RunTrust.Decision, RunTrust.Approval) {
        let docEpoch = (try? await app?.cache?.doc(c.doc))?.currentEpoch ?? 0
        // nil when offline: then only an untouched approved doc runs without asking
        let history = try? await app?.api?.docHistory(c.doc)
        let me = await currentMe()
        let decision = RunTrust.decide(
            block: c.block, history: history, me: me, practiceEditedByMe: practiceEdited,
            approval: approvals?.approval(for: c.doc), docEpoch: docEpoch
        )
        return (decision, RunTrust.approval(history: history, me: me, docEpoch: docEpoch))
    }

    /// The signed-in person (`/api/profile`), once per server.
    private func currentMe() async -> RunTrust.Me {
        if let me, meServer == app?.serverURL { return me }
        guard let app, let api = app.api, let p = try? await api.profile() else {
            return RunTrust.Me(principalID: nil, name: nil)
        }
        let m = RunTrust.Me(principalID: p.principalID?.lowercased(), name: p.name)
        me = m
        meServer = app.serverURL
        return m
    }

    // MARK: practice edits

    func beginPractice(_ s: BlockRunState, docCode: String) {
        if s.practice == nil { s.practice = docCode }
        s.saveError = nil
    }

    func revert(_ s: BlockRunState) {
        s.practice = nil
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
            s.practice = nil
            s.saveError = nil
        } catch {
            s.saveError = error.localizedDescription
        }
    }
}
