import Foundation
import SwiftUI
import UIKit
import Testing
import TaisceKit
@testable import Taisce
#if targetEnvironment(macCatalyst)
import Darwin
#endif

/// Runnable code blocks in the app: the status line, the per-block session
/// state, the paths a test host uses, and (Mac) real runs in this process.
@MainActor @Suite struct RunCodeAppTests {
    @Test func statusLines() {
        #expect(RunStatus.line(RunResult(exitCode: 0, duration: .milliseconds(420)), compiledOnly: false) == ("exit 0 · 0.42 s", true))
        #expect(RunStatus.line(RunResult(exitCode: 2, duration: .seconds(12)), compiledOnly: false) == ("exit 2 · 12.0 s", false))
        #expect(RunStatus.line(RunResult(exitCode: 0, duration: .seconds(1), goShape: .declarations), compiledOnly: true) == ("compiled \u{2713} · 1.00 s", true))
        #expect(RunStatus.line(RunResult(signal: 15, duration: .seconds(1), stopped: true), compiledOnly: false).text == "Stopped · 1.00 s")
        #expect(RunStatus.line(RunResult(signal: 15, duration: .seconds(300), timedOut: true), compiledOnly: false).text == "Timed out after 5 min 0 s (limit 5 min)")
        #expect(RunStatus.line(RunResult(message: RunMessages.noGo), compiledOnly: false) == (RunMessages.noGo, false))
        #expect(RunStatus.line(RunResult(exitCode: 1, buildFailed: true), compiledOnly: false).text.hasPrefix("Build failed"))
    }

    @Test func sessionStatePerBlock() {
        let store = CodeRunStore()
        let a = CodeRunContext(doc: "d", block: "b1", canSave: true)
        let s = store.state(a)
        s.tryLine = "f(1)"
        #expect(store.state(a) === s, "the same block keeps its state (try line) for the session")
        #expect(store.state(CodeRunContext(doc: "d", block: "b2", canSave: true)) !== s)
        // practice text: only a real change counts as yours
        store.beginPractice(s, docCode: "echo hi")
        #expect(s.practice == "echo hi" && !s.isPracticeEdited)
        s.practice = "echo bye"
        #expect(s.isPracticeEdited && s.code(doc: "echo hi") == "echo bye")
        store.revert(s)
        #expect(s.practice == nil && s.code(doc: "echo hi") == "echo hi")
        store.reset()
        #expect(store.state(a) !== s)
    }

    @Test func aTestHostKeepsAwayFromTheRealData() throws {
        #expect(AppPaths.isTestHost)
        let support = try AppPaths.supportDirectory()
        #expect(support.path.hasPrefix(FileManager.default.temporaryDirectory.path))
        #expect(AppPaths.diagramCache.path.hasPrefix(FileManager.default.temporaryDirectory.path))
        #expect(AppPaths.migrateSandboxContainer() == nil, "never migrates inside a test host")
    }

    /// A booted model on an unreachable LOCAL server, its cache in the test
    /// host's temp folder, holding one doc with a bash block.
    func modelWithCodeDoc() async throws -> (AppModel, DocID, BlockID) {
        // never boot on whatever server UserDefaults holds (other suites set it):
        // switch straight to an unreachable LOCAL one
        let m = AppModel()
        m.discover = { _ in nil }
        await m.setServerURL("http://127.0.0.1:9")
        // already on :9 (another suite's setting): setServerURL was a no-op
        if m.cache == nil { await m.boot() }
        await m.stopSync()
        UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey)
        #expect(m.serverURL == "http://127.0.0.1:9")
        let cache = try #require(m.cache)
        let doc = "run-\(UUID().uuidString.prefix(8))".lowercased()
        let block = "\(doc)-b"
        try await cache.storeDoc(DocTree(
            doc: DocSummary(id: doc, parentID: nil, title: "Runs", currentEpoch: 2),
            roots: [BlockNode(block: Block(id: block, docID: doc, parentID: nil, orderKey: "a", blockType: .code, content: "```bash cwd=~\necho hi\n```", epoch: 2))]
        ))
        return (m, doc, block)
    }

    @Test func theCodeCardLaysOut() async throws {
        let (m, doc, block) = try await modelWithCodeDoc()
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.codeRuns.state(ctx)
        s.log.append([OutputChunk(.stdout, "out\n"), OutputChunk(.stderr, "err\n")])
        s.result = RunResult(exitCode: 1, duration: .seconds(1))
        s.tryLine = "f(1)"
        try AppModelTests().render(
            VStack {
                RunnableCodeBlock(language: "bash", code: "echo hi", attributes: ["cwd": "~"])
                RunnableCodeBlock(language: "go", code: "func f(x int) int { return x }")
                RunnableCodeBlock(language: "python", code: "print(1)")
            }
            .environment(m)
            .environment(\.codeRunContext, ctx)
        )
        store(m).beginPractice(s, docCode: "echo hi")
        try AppModelTests().render(RunnableCodeBlock(language: "bash", code: "echo hi").environment(m).environment(\.codeRunContext, ctx))
        try await m.cache?.deleteDoc(doc)
    }

    func store(_ m: AppModel) -> CodeRunStore { m.codeRuns }

    @Test func saveToDocQueuesAReplaceThroughTheOutbox() async throws {
        let (m, doc, block) = try await modelWithCodeDoc()
        let cache = try #require(m.cache)
        let before = try await cache.pendingOutbox().count
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.codeRuns.state(ctx)
        m.codeRuns.beginPractice(s, docCode: "echo hi")
        s.practice = "echo bye"
        await m.codeRuns.save(s, context: ctx)
        #expect(s.saveError == nil && s.practice == nil)
        let queued = try await cache.pendingOutbox()
        #expect(queued.count == before + 1)
        let body = try #require(queued.last?.body)
        let req = try JSONDecoder().decode(ProposeRequest.self, from: body)
        #expect(req.docID == doc && req.baseEpoch == 2)
        #expect(String(decoding: body, as: UTF8.self).contains(#"```bash cwd=~\necho bye\n```"#))
        try await cache.deleteDoc(doc)
        _ = try await cache.dropOutbox(forDocs: [doc])
    }

    #if targetEnvironment(macCatalyst)
    @Test func offlineRunAsksFirstThenRunsAndRemembers() async throws {
        let (m, doc, block) = try await modelWithCodeDoc()
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.codeRuns.state(ctx)
        let req = RunRequest(language: .shell(interpreter: "/bin/bash"), code: "echo hi", cwd: "~")
        // nothing listens on :9, so no ledger: who wrote it is unknown → ask
        await m.codeRuns.run(s, req, context: ctx, docCode: "echo hi")
        let prompt = try #require(s.trustPrompt)
        #expect(prompt.lastEditedBy.hasPrefix("someone (couldn't check who: "), "\(prompt.lastEditedBy)")
        #expect(prompt.code == "echo hi")
        #expect(!s.isRunning && !s.isChecking)
        // the sheet's Run button, exactly as wired: its dismiss goes through
        // the sheet's binding (which clears the prompt) — the run still starts
        let binding = TrustFlow.presentation(m.codeRuns, s)
        #expect(binding.wrappedValue?.id == prompt.id)
        await TrustFlow.runPressed(m.codeRuns, s, prompt) { binding.wrappedValue = nil }.value
        #expect(s.trustPrompt == nil)
        #expect(s.result?.exitCode == 0)
        #expect(s.log.text == "hi\n")
        // approved for this doc at this epoch: the next run doesn't ask
        await m.codeRuns.run(s, req, context: ctx, docCode: "echo hi")
        #expect(s.trustPrompt == nil && s.result?.exitCode == 0)
        // your own practice edit never asks, even in a doc never approved
        let other = CodeRunContext(doc: "\(doc)-2", block: "b", canSave: false)
        let s2 = m.codeRuns.state(other)
        m.codeRuns.beginPractice(s2, docCode: "echo a")
        s2.practice = "echo typed"
        await m.codeRuns.run(s2, RunRequest(language: .shell(interpreter: "/bin/sh"), code: "echo typed"), context: other, docCode: "echo a")
        #expect(s2.trustPrompt == nil && s2.log.text == "typed\n")
        RunApprovals(server: m.serverURL).forget()
        try await m.cache?.deleteDoc(doc)
    }
    #endif

    /// The order 38ccf95 used (dismiss first, then run) works too, and
    /// Cancel starts nothing; a prompt left over never wedges the block.
    @Test func trustSheetButtonsInEitherOrder() async throws {
        let (m, doc, block) = try await modelWithCodeDoc()
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.codeRuns.state(ctx)
        let req = RunRequest(language: .shell(interpreter: "/bin/bash"), code: "echo hi")
        let binding = TrustFlow.presentation(m.codeRuns, s)
        await m.codeRuns.run(s, req, context: ctx, docCode: "echo hi")
        let p1 = try #require(s.trustPrompt)
        TrustFlow.cancelPressed(m.codeRuns, s) { binding.wrappedValue = nil }
        #expect(s.trustPrompt == nil && s.result == nil && !s.isBusy)
        // a stale prompt (set, never shown) is replaced by the next Run
        s.trustPrompt = p1
        await m.codeRuns.run(s, req, context: ctx, docCode: "echo hi")
        let p2 = try #require(s.trustPrompt)
        #expect(p2.id != p1.id)
        binding.wrappedValue = nil
        await m.codeRuns.confirm(s, p2)
        #if targetEnvironment(macCatalyst)
        #expect(s.result?.exitCode == 0 && s.log.text == "hi\n")
        #else
        #expect(s.result?.message == "Code runs on the Mac.")
        #endif
        RunApprovals(server: m.serverURL).forget()
        try await m.cache?.deleteDoc(doc)
    }

    /// A slow server: "Checking who wrote this…" shows at once, and after
    /// the limit it asks anyway, saying why.
    @Test func aSlowTrustCheckAsksAfterTheLimit() async throws {
        let (m, doc, block) = try await modelWithCodeDoc()
        // a server that accepts and never answers
        let listener = try SilentListener()
        defer { listener.close() }
        await m.setServerURL("http://127.0.0.1:\(listener.port)")
        await m.stopSync()
        UserDefaults.standard.removeObject(forKey: AppModel.serverURLKey)
        m.codeRuns.trustCheckLimit = .milliseconds(400)
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.codeRuns.state(ctx)
        let running = Task { await m.codeRuns.run(s, RunRequest(language: .shell(interpreter: "/bin/bash"), code: "echo hi"), context: ctx, docCode: "echo hi") }
        try await Task.sleep(for: .milliseconds(100))
        #expect(s.isChecking && s.phase == "Checking who wrote this\u{2026}" && s.hasOutput)
        await running.value
        #expect(!s.isChecking)
        #expect(s.trustPrompt?.lastEditedBy.contains("didn't answer") == true, "\(String(describing: s.trustPrompt?.lastEditedBy))")
    }

    /// S1: practice text opened on v1 and left untouched; the doc moves to
    /// v2 (someone else's): Run runs v2, with v2's trust, never v1.
    @Test func untouchedPracticeTextFollowsTheDoc() async throws {
        let (m, doc, block) = try await modelWithCodeDoc()
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let s = m.codeRuns.state(ctx)
        m.codeRuns.beginPractice(s, docCode: "echo v1")
        #expect(!s.isPracticeEdited)
        await m.codeRuns.run(s, RunRequest(language: .shell(interpreter: "/bin/bash"), code: s.code(doc: "echo v2")), context: ctx, docCode: "echo v2")
        let p = try #require(s.trustPrompt, "untouched practice text is the doc's: it asks")
        #expect(p.code == "echo v2" && p.request.code == "echo v2")
        #expect(s.practice == "echo v2")
        // typed text is yours: it runs as typed, no question
        s.trustPrompt = nil
        s.practice = "echo mine"
        #expect(s.isPracticeEdited)
        s.followDoc("echo v3")
        #expect(s.practice == "echo mine", "typed text never follows the doc")
        await m.codeRuns.run(s, RunRequest(language: .shell(interpreter: "/bin/bash"), code: "echo mine"), context: ctx, docCode: "echo v3")
        #expect(s.trustPrompt == nil)
        try await m.cache?.deleteDoc(doc)
    }

    #if targetEnvironment(macCatalyst)
    /// The card in a real window: Run on a block nobody can vouch for
    /// presents the trust sheet (a UIKit presentation, not just state).
    @Test func runPresentsTheTrustSheetOnScreen() async throws {
        let (m, doc, block) = try await modelWithCodeDoc()
        let ctx = CodeRunContext(doc: doc, block: block, canSave: true)
        let scene = try #require(UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.first)
        let window = UIWindow(windowScene: scene)
        let host = UIHostingController(rootView: AnyView(
            RunnableCodeBlock(language: "bash", code: "echo hi").environment(m).environment(\.codeRunContext, ctx)
        ))
        window.rootViewController = host
        window.makeKeyAndVisible()
        defer { window.isHidden = true }
        host.view.layoutIfNeeded()
        let s = m.codeRuns.state(ctx)
        // what the Run button calls
        await m.codeRuns.run(s, RunRequest(language: .shell(interpreter: "/bin/bash"), code: "echo hi"), context: ctx, docCode: "echo hi")
        #expect(s.trustPrompt != nil)
        for _ in 0..<100 where host.presentedViewController == nil {
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(host.presentedViewController != nil, "the sheet is on screen")
        // its Run button's action, then the sheet goes and the run happens
        let p = try #require(s.trustPrompt)
        await TrustFlow.runPressed(m.codeRuns, s, p) { s.trustPrompt = nil }.value
        for _ in 0..<100 where host.presentedViewController != nil {
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(host.presentedViewController == nil)
        #expect(s.result?.exitCode == 0 && s.log.text == "hi\n")
        RunApprovals(server: m.serverURL).forget()
        try await m.cache?.deleteDoc(doc)
    }
    #endif

    @Test func approvalsAreForgottenWithThePersonsSettings() throws {
        let suite = "taisce.tests.run.\(UUID().uuidString)"
        let d = try #require(UserDefaults(suiteName: suite))
        defer { d.removePersistentDomain(forName: suite) }
        let settings = UserSettings(defaults: d, server: "https://taisce.test")
        RunApprovals(defaults: d, server: "https://taisce.test").approve("doc", .init(docEpoch: 1, othersEpoch: nil))
        #expect(d.object(forKey: settings.runApprovalsKey) != nil)
        settings.forget()
        #expect(d.object(forKey: settings.runApprovalsKey) == nil)
    }
}

#if targetEnvironment(macCatalyst)
/// The runner inside the Mac Catalyst app (unsandboxed, posix_spawn): the
/// brief's integration cases. Go cases skip when `go` isn't on the login PATH.
@Suite(.serialized) struct RunnerTests {
    struct Outcome {
        var log = OutputLog()
        var result: RunResult?
        var events = 0
    }

    static func options(timeout: Duration = .seconds(60)) throws -> CodeRun.Options {
        var o = CodeRun.Options()
        o.timeout = timeout
        o.tempRoot = FileManager.default.temporaryDirectory.appendingPathComponent("taisce-app-runner-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: o.tempRoot, withIntermediateDirectories: true)
        return o
    }

    static func collect(_ run: CodeRun) async -> Outcome {
        var o = Outcome()
        for await e in run.events {
            o.events += 1
            switch e {
            case .phase: break
            case .output(let c): o.log.append(c)
            case .truncated: o.log.truncated = true
            case .finished(let r): o.result = r
            }
        }
        return o
    }

    static func env() async -> [String: String] { await LoginEnvironmentCache.shared.environment() }

    static let pairSum = """
    func pairSum(nums []int, target int) (int, int, bool) {
    \tl, r := 0, len(nums)-1
    \tfor l < r {
    \t\ts := nums[l] + nums[r]
    \t\tif s == target {
    \t\t\treturn l, r, true
    \t\t} else if s < target {
    \t\t\tl++
    \t\t} else {
    \t\t\tr--
    \t\t}
    \t}
    \treturn -1, -1, false
    }
    """

    @Test func theAppIsUnsandboxed() {
        #expect(ProcessInfo.processInfo.environment["APP_SANDBOX_CONTAINER_ID"] == nil)
        #expect(!AppPaths.isSandboxed)
    }

    @Test func echoHi() async throws {
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "echo hi"), environment: await Self.env(), options: try Self.options()))
        #expect(o.log.text == "hi\n")
        #expect(o.result?.exitCode == 0)
    }

    @Test func stderrAndANonZeroExit() async throws {
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/zsh"), code: "echo oops >&2\nexit 7"), environment: await Self.env(), options: try Self.options()))
        #expect(o.log.chunks == [OutputChunk(.stderr, "oops\n")])
        #expect(o.result?.exitCode == 7)
    }

    @Test func stopLeavesNoOrphan() async throws {
        let marker = FileManager.default.temporaryDirectory.appendingPathComponent("pid-\(UUID().uuidString)")
        let run = CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "sleep 30 &\necho $! > '\(marker.path)'\nwait"), environment: await Self.env(), options: try Self.options())
        let collecting = Task { await Self.collect(run) }
        var pid: pid_t = 0
        for _ in 0..<250 {
            if let s = try? String(contentsOf: marker, encoding: .utf8), let p = pid_t(s.trimmingCharacters(in: .whitespacesAndNewlines)) {
                pid = p
                break
            }
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(pid > 0)
        #expect(kill(pid, 0) == 0)
        run.stop()
        let o = await collecting.value
        #expect(o.result?.stopped == true)
        var gone = false
        for _ in 0..<50 where !gone {
            gone = kill(pid, 0) == -1 && errno == ESRCH
            if !gone { try await Task.sleep(for: .milliseconds(20)) }
        }
        #expect(gone, "the backgrounded sleep is gone too")
    }

    @Test func timeoutFires() async throws {
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/sh"), code: "sleep 30"), environment: await Self.env(), options: try Self.options(timeout: .milliseconds(400))))
        #expect(o.result?.timedOut == true)
        #expect((o.result?.duration ?? .zero) < .seconds(4))
    }

    @Test func goPairSumFromTheTryLine() async throws {
        let env = await Self.env()
        guard LoginEnvironment.which("go", path: env["PATH"]) != nil else { return }
        let opts = try Self.options(timeout: .seconds(120))
        // 2+7: indices 1 and 3 (the brief's `1 3 true`; for target 6 it is 2+4, `1 2 true`)
        let nine = await Self.collect(CodeRun(RunRequest(language: .go, code: Self.pairSum, tryLine: "pairSum([]int{1,2,4,7}, 9)"), environment: env, options: opts))
        #expect(nine.log.text == "1 3 true\n", "\(nine.log.text)")
        let six = await Self.collect(CodeRun(RunRequest(language: .go, code: Self.pairSum, tryLine: "pairSum([]int{1,2,4,7}, 6)"), environment: env, options: opts))
        #expect(six.log.text == "1 2 true\n", "\(six.log.text)")
    }

    @Test func aHundredThousandLinesStayResponsiveAndAreCapped() async throws {
        // the main actor keeps answering while the output pours in
        let ticks = MainActorTicks()
        let pinger = Task { @MainActor in
            while !Task.isCancelled {
                ticks.count += 1
                try? await Task.sleep(for: .milliseconds(10))
            }
        }
        let start = ContinuousClock.now
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "for i in $(seq 1 100000); do echo \"line $i\"; done"), environment: await Self.env(), options: try Self.options()))
        let took = ContinuousClock.now - start
        pinger.cancel()
        #expect(o.result?.exitCode == 0)
        #expect(o.log.truncated == (o.log.text.utf8.count >= OutputCollector.defaultCap))
        #expect(o.log.text.utf8.count <= OutputCollector.defaultCap)
        #expect(o.events <= Int(took / .milliseconds(50)) + 4, "\(o.events) batches in \(took)")
        #expect(await ticks.count >= Int(took / .milliseconds(10)) / 4, "the main actor kept running")
        // over the cap for real: 100k lines of 20 bytes
        let big = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "for i in $(seq 1 100000); do echo \"line $i of 100000 ...\"; done"), environment: await Self.env(), options: try Self.options()))
        #expect(big.log.truncated)
        #expect(big.log.text.utf8.count == OutputCollector.defaultCap)
    }
}

@MainActor final class MainActorTicks {
    var count = 0
}
#endif

/// A TCP listener on loopback that accepts and never answers.
final class SilentListener: @unchecked Sendable {
    let fd: Int32
    let port: UInt16
    var accepted: [Int32] = []

    init() throws {
        let sock = socket(AF_INET, SOCK_STREAM, 0)
        var addr = sockaddr_in()
        addr.sin_family = sa_family_t(AF_INET)
        addr.sin_addr.s_addr = inet_addr("127.0.0.1")
        addr.sin_port = 0
        var len = socklen_t(MemoryLayout<sockaddr_in>.size)
        let ok = withUnsafeMutablePointer(to: &addr) { p in
            p.withMemoryRebound(to: sockaddr.self, capacity: 1) { bind(sock, $0, len) == 0 && listen(sock, 8) == 0 && getsockname(sock, $0, &len) == 0 }
        }
        guard ok else { throw POSIXError(.EADDRINUSE) }
        fd = sock
        port = UInt16(bigEndian: addr.sin_port)
    }

    func close() {
        Darwin.close(fd)
    }
}
