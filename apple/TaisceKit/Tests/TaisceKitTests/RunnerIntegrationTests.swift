#if os(macOS)
import Darwin
import Foundation
import Synchronization
import Testing
@testable import TaisceKit

/// Real processes on this Mac (the same runner the Mac Catalyst app uses;
/// `TaisceTests/RunnerTests` repeats these inside the app). Each run lives in
/// a temp dir under the test's own root.
@Suite(.serialized) struct RunnerIntegrationTests {
    struct Outcome {
        var log = OutputLog()
        var result: RunResult?
        var events = 0
        var phases: [String] = []
    }

    static func options(timeout: Duration = .seconds(60)) throws -> CodeRun.Options {
        var o = CodeRun.Options()
        o.timeout = timeout
        o.tempRoot = FileManager.default.temporaryDirectory.appendingPathComponent("taisce-runner-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: o.tempRoot, withIntermediateDirectories: true)
        return o
    }

    static func collect(_ run: CodeRun, onStart: (@Sendable () -> Void)? = nil) async -> Outcome {
        var o = Outcome()
        for await e in run.events {
            o.events += 1
            switch e {
            case .phase(let p): o.phases.append(p)
            case .output(let c): o.log.append(c)
            case .truncated: o.log.truncated = true
            case .finished(let r): o.result = r
            }
        }
        return o
    }

    static func env() async -> [String: String] { await LoginEnvironmentCache.shared.environment() }

    @Test func echoPrintsAndExitsZero() async throws {
        let opts = try Self.options()
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "echo hi"), environment: await Self.env(), options: opts))
        #expect(o.log.text == "hi\n")
        #expect(o.result?.exitCode == 0)
        #expect(o.result?.succeeded == true)
        // the run's temp dir is gone afterwards
        #expect(try FileManager.default.contentsOfDirectory(atPath: opts.tempRoot.appendingPathComponent("taisce-run").path).isEmpty)
    }

    @Test func stderrIsCapturedAndTheExitCodeKept() async throws {
        let code = "echo out\necho err >&2\nexit 3"
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/sh"), code: code), environment: await Self.env(), options: try Self.options()))
        #expect(o.log.chunks.contains(OutputChunk(.stderr, "err\n")))
        #expect(o.log.chunks.contains(OutputChunk(.stdout, "out\n")))
        #expect(o.result?.exitCode == 3)
        #expect(o.result?.succeeded == false)
    }

    @Test func cwdAttributeAndLoginPath() async throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("cwd-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let real = realpath(dir.path, nil).map { p in defer { free(p) }; return String(cString: p) } ?? dir.path
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/zsh"), code: "pwd -P; [ -t 0 ] && echo tty || echo no-stdin", cwd: dir.path), environment: await Self.env(), options: try Self.options()))
        #expect(o.log.text == "\(real)\nno-stdin\n")
        let missing = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "pwd", cwd: "~/no-such-dir-\(UUID().uuidString)"), environment: await Self.env(), options: try Self.options()))
        #expect(missing.result?.message?.hasPrefix("The cwd ~/no-such-dir-") == true)
        let env = await Self.env()
        #expect(env["PATH"]?.isEmpty == false)
        #expect(env["HOME"] == Account.current.home)
    }

    @Test func stopKillsTheWholeGroup() async throws {
        let marker = FileManager.default.temporaryDirectory.appendingPathComponent("pid-\(UUID().uuidString)")
        // a child in the background, its pid written down, the shell waiting on it
        let code = "sleep 30 &\necho $! > '\(marker.path)'\nwait"
        let run = CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: code), environment: await Self.env(), options: try Self.options())
        let collecting = Task { await Self.collect(run) }
        var pid: pid_t = 0
        for _ in 0..<200 {
            if let s = try? String(contentsOf: marker, encoding: .utf8), let p = pid_t(s.trimmingCharacters(in: .whitespacesAndNewlines)) {
                pid = p
                break
            }
            try await Task.sleep(for: .milliseconds(20))
        }
        #expect(pid > 0)
        #expect(kill(pid, 0) == 0, "the background sleep is running")
        let start = ContinuousClock.now
        run.stop()
        let o = await collecting.value
        #expect(ContinuousClock.now - start < .seconds(4))
        #expect(o.result?.stopped == true)
        #expect(await Self.gone(pid), "no orphaned sleep")
    }

    /// Up to 1 s for a pid to be gone (an orphan is reaped by launchd, not us).
    static func gone(_ pid: pid_t) async -> Bool {
        for _ in 0..<50 {
            if kill(pid, 0) == -1 && errno == ESRCH { return true }
            try? await Task.sleep(for: .milliseconds(20))
        }
        return false
    }

    /// S3: a backgrounded child that let go of the pipes is ended with the
    /// run, every time (not only when the waiter happened to see the leader
    /// go first).
    @Test func aDetachedBackgroundChildNeverOutlivesTheRun() async throws {
        for round in 0..<8 {
            let marker = FileManager.default.temporaryDirectory.appendingPathComponent("bg-\(UUID().uuidString)")
            let code = "sleep 20 >/dev/null 2>&1 &\necho $! > '\(marker.path)'\necho done"
            let run = CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: code), environment: await Self.env(), options: try Self.options())
            let o = await Self.collect(run)
            #expect(o.result?.exitCode == 0 && o.log.text == "done\n")
            let pid = pid_t((try? String(contentsOf: marker, encoding: .utf8))?.trimmingCharacters(in: .whitespacesAndNewlines) ?? "") ?? 0
            #expect(pid > 0)
            let isGone = await Self.gone(pid)
            #expect(isGone, "round \(round): the background sleep \(pid) outlived the run")
            if !isGone { kill(pid, SIGKILL) }
            // N3: the finished run holds no process: a late Stop signals nothing
            #expect(run.state.withLock { $0.current } == nil)
            run.stop()
        }
    }

    @Test func staleRunDirectoriesAreSwept() throws {
        let opts = try Self.options()
        let stale = opts.tempRoot.appendingPathComponent("taisce-run/\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: stale, withIntermediateDirectories: true)
        try Data("x".utf8).write(to: stale.appendingPathComponent("block.sh"))
        CodeRun.sweepStaleRuns(tempRoot: opts.tempRoot)
        #expect(try FileManager.default.contentsOfDirectory(atPath: opts.tempRoot.appendingPathComponent("taisce-run").path).isEmpty)
        CodeRun.sweepStaleRuns(tempRoot: opts.tempRoot.appendingPathComponent("nothing-here"))
    }

    @Test func aTrappedTermIsKilledAfterTheGrace() async throws {
        let code = "trap '' TERM\necho ready\nwhile true; do sleep 0.1; done"
        let run = CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: code), environment: await Self.env(), options: try Self.options())
        let collecting = Task { await Self.collect(run) }
        try await Task.sleep(for: .milliseconds(300))
        let start = ContinuousClock.now
        run.stop()
        let o = await collecting.value
        let took = ContinuousClock.now - start
        #expect(took >= .seconds(1.8) && took < .seconds(5), "SIGKILL after the 2 s grace (\(took))")
        #expect(o.result?.signal == SIGKILL)
    }

    @Test func timeoutFires() async throws {
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/sh"), code: "echo start\nsleep 30"), environment: await Self.env(), options: try Self.options(timeout: .milliseconds(1500))))
        #expect(o.result?.timedOut == true)
        #expect(o.result?.stopped == false)
        #expect(o.log.text == "start\n")
        #expect((o.result?.duration ?? .zero) < .seconds(6))
    }

    @Test func chattyOutputIsBatchedAndCapped() async throws {
        let start = ContinuousClock.now
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "for i in $(seq 1 100000); do echo \"line $i of a chatty program\"; done"), environment: await Self.env(), options: try Self.options()))
        let took = ContinuousClock.now - start
        #expect(o.result?.exitCode == 0)
        #expect(o.log.truncated)
        #expect(o.log.text.utf8.count == OutputCollector.defaultCap)
        // one event per ~50 ms tick at most (plus phase, truncated, finished)
        let ticks = Int(took / .milliseconds(50)) + 4
        #expect(o.events <= ticks, "\(o.events) events in \(took)")
    }

    @Test func ansiColoursAreStripped() async throws {
        let o = await Self.collect(CodeRun(RunRequest(language: .shell(interpreter: "/bin/bash"), code: "printf '\\033[1;32mgreen\\033[0m\\n'"), environment: await Self.env(), options: try Self.options()))
        #expect(o.log.text == "green\n")
    }

    // MARK: Go

    static func goAvailable() async -> Bool {
        LoginEnvironment.which("go", path: await env()["PATH"]) != nil
    }

    @Test func goPairSumFromTheTryLine() async throws {
        guard await Self.goAvailable() else { return }
        let env = await Self.env()
        let opts = try Self.options(timeout: .seconds(120))
        let o = await Self.collect(CodeRun(RunRequest(language: .go, code: GoProgramTests.pairSum, tryLine: "pairSum([]int{1,2,4,7}, 9)"), environment: env, options: opts))
        #expect(o.log.text == "1 3 true\n", "\(o.log.text)")
        #expect(o.result?.exitCode == 0 && o.result?.goShape == .declarations)
        let six = await Self.collect(CodeRun(RunRequest(language: .go, code: GoProgramTests.pairSum, tryLine: "pairSum([]int{1,2,4,7}, 6)"), environment: env, options: opts))
        #expect(six.log.text == "1 2 true\n")
        // an empty try line: it compiles, prints nothing
        let compiled = await Self.collect(CodeRun(RunRequest(language: .go, code: GoProgramTests.pairSum), environment: env, options: opts))
        #expect(compiled.log.isEmpty && compiled.result?.succeeded == true)
    }

    @Test func goStatementsWithInferredAndPrunedImports() async throws {
        guard await Self.goAvailable() else { return }
        let env = await Self.env()
        let opts = try Self.options(timeout: .seconds(120))
        // `strings` is written but unused once wrapped: dropped, not an error
        let code = "import \"strings\"\nxs := []int{3, 1, 2}\nsort.Ints(xs)\nfmt.Println(xs)"
        let o = await Self.collect(CodeRun(RunRequest(language: .go, code: code), environment: env, options: opts))
        #expect(o.log.text == "[1 2 3]\n", "\(o.log.text)")
        let broken = await Self.collect(CodeRun(RunRequest(language: .go, code: "fmt.Println(undefinedThing)"), environment: env, options: opts))
        #expect(broken.result?.buildFailed == true)
        #expect(broken.log.text.contains("undefined: undefinedThing"))
        #expect(!broken.log.text.contains(opts.tempRoot.path))
    }

    @Test func goMissingSaysSo() async throws {
        var env = await Self.env()
        env["PATH"] = "/usr/bin:/bin"
        let o = await Self.collect(CodeRun(RunRequest(language: .go, code: "fmt.Println(1)"), environment: env, options: try Self.options()))
        #expect(o.result?.message == RunMessages.noGo)
    }
}
#endif
