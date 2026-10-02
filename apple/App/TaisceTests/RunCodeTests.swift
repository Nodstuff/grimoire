import Foundation
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
        #expect(s.practice == "echo hi" && !s.isPracticeEdited(doc: "echo hi"))
        s.practice = "echo bye"
        #expect(s.isPracticeEdited(doc: "echo hi") && s.code(doc: "echo hi") == "echo bye")
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
        #expect(kill(pid, 0) == -1 && errno == ESRCH, "the backgrounded sleep is gone too")
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
