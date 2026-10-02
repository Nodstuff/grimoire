import Foundation
import Synchronization

/// Which fences get a Run control, and how they run.
public enum RunnableLanguage: Sendable, Hashable {
    case shell(interpreter: String)
    case go

    public init?(_ language: String?) {
        switch language?.lowercased() {
        case "bash": self = .shell(interpreter: "/bin/bash")
        case "sh": self = .shell(interpreter: "/bin/sh")
        case "zsh": self = .shell(interpreter: "/bin/zsh")
        case "go", "golang": self = .go
        default: return nil
        }
    }

    public var isGo: Bool { self == .go }
}

public struct RunRequest: Sendable, Hashable {
    public var language: RunnableLanguage
    public var code: String
    /// the fence's `cwd=` (unexpanded); nil = the run's own temp dir
    public var cwd: String?
    /// Go declarations blocks: the expression `main` prints
    public var tryLine: String

    public init(language: RunnableLanguage, code: String, cwd: String? = nil, tryLine: String = "") {
        self.language = language
        self.code = code
        self.cwd = cwd
        self.tryLine = tryLine
    }
}

/// How a run ended.
public struct RunResult: Sendable, Hashable {
    public var exitCode: Int32?
    public var signal: Int32?
    public var duration: Duration
    public var timedOut = false
    public var stopped = false
    /// why it never ran: no `go`, a bad `cwd=`, a spawn failure
    public var message: String?
    /// a Go block's shape (nil for shell)
    public var goShape: GoProgram.Shape?
    /// the build failed (the compiler's output is in the log)
    public var buildFailed = false

    public init(exitCode: Int32? = nil, signal: Int32? = nil, duration: Duration = .zero, timedOut: Bool = false, stopped: Bool = false, message: String? = nil, goShape: GoProgram.Shape? = nil, buildFailed: Bool = false) {
        self.exitCode = exitCode
        self.signal = signal
        self.duration = duration
        self.timedOut = timedOut
        self.stopped = stopped
        self.message = message
        self.goShape = goShape
        self.buildFailed = buildFailed
    }

    public var succeeded: Bool { exitCode == 0 && !timedOut && !stopped && message == nil }
}

public enum RunEvent: Sendable, Hashable {
    /// "Building…", "Running…"
    case phase(String)
    case output([OutputChunk])
    case truncated
    case finished(RunResult)
}

public enum RunMessages {
    public static let noGo = "Go isn't installed (no `go` on your PATH)"
    public static func noDirectory(_ path: String) -> String { "The cwd \(path) doesn't exist" }
}

#if os(macOS) || targetEnvironment(macCatalyst)
import Darwin

/// The login environment, captured once per launch: `$SHELL -l -c 'env -0'`
/// from a minimal seed, bounded at 5 s, falling back to this process's own.
public actor LoginEnvironmentCache {
    public static let shared = LoginEnvironmentCache()
    var captured: [String: String]?
    var inFlight: Task<[String: String], Never>?
    let timeout: Duration

    public init(timeout: Duration = .seconds(5)) {
        self.timeout = timeout
    }

    public func environment() async -> [String: String] {
        if let captured { return captured }
        if let inFlight { return await inFlight.value }
        let timeout = timeout
        let t = Task.detached { await Self.capture(timeout: timeout) }
        inFlight = t
        let env = await t.value
        captured = env
        inFlight = nil
        return env
    }

    static func capture(timeout: Duration) async -> [String: String] {
        let me = Account.current
        let seed = LoginEnvironment.seed(home: me.home, user: me.user, shell: me.shell, tmpdir: ProcessInfo.processInfo.environment["TMPDIR"])
        let spec = SpawnSpec(executable: me.shell, arguments: ["-l", "-c", "env -0"], environment: seed, directory: me.home)
        if let r = try? await ProcessCapture.run(spec, timeout: timeout), !r.timedOut, r.status.exitCode == 0 {
            let env = LoginEnvironment.parse(r.stdout)
            if env["PATH"] != nil { return env }
        }
        var env = ProcessInfo.processInfo.environment
        for k in LoginEnvironment.dropped { env[k] = nil }
        return env
    }
}

/// The signed-in macOS user, from the password database (unaffected by
/// the app's own environment).
public struct Account: Sendable {
    public var user: String
    public var home: String
    public var shell: String

    public static var current: Account {
        let env = ProcessInfo.processInfo.environment
        if let pw = getpwuid(getuid()) {
            let user = String(cString: pw.pointee.pw_name)
            let home = String(cString: pw.pointee.pw_dir)
            var shell = String(cString: pw.pointee.pw_shell)
            if shell.isEmpty || !FileManager.default.isExecutableFile(atPath: shell) { shell = env["SHELL"] ?? "/bin/zsh" }
            return Account(user: user, home: home, shell: shell)
        }
        return Account(user: env["USER"] ?? "", home: NSHomeDirectory(), shell: env["SHELL"] ?? "/bin/zsh")
    }
}

/// One run of one block: its own process (group) in its own temp dir,
/// deleted afterwards. Events arrive on `events` (output batched every
/// `batchInterval`); `stop()` ends it (SIGTERM to the group, SIGKILL 2 s
/// later).
public final class CodeRun: Sendable {
    public let events: AsyncStream<RunEvent>
    let continuation: AsyncStream<RunEvent>.Continuation
    let state = Mutex(State())

    struct State {
        var current: SpawnedProcess?
        var stopped = false
        var collector = OutputCollector()
        var sentTruncated = false
    }

    public struct Options: Sendable {
        public var timeout: Duration = .seconds(300)
        public var cap: Int = OutputCollector.defaultCap
        public var batchInterval: Duration = .milliseconds(50)
        /// runs live under `<tempRoot>/taisce-run/<uuid>`
        public var tempRoot: URL = URL(fileURLWithPath: NSTemporaryDirectory())
        public var home: String = Account.current.home

        public init() {}
    }

    let options: Options

    public init(_ request: RunRequest, environment: [String: String], options: Options = Options()) {
        self.options = options
        (events, continuation) = AsyncStream.makeStream(of: RunEvent.self, bufferingPolicy: .unbounded)
        state.withLock { $0.collector = OutputCollector(cap: options.cap) }
        let run = self
        Task.detached { await run.execute(request, environment: environment) }
    }

    public func stop() {
        let p = state.withLock { s in
            s.stopped = true
            return s.current
        }
        p?.terminateGroup()
    }

    var isStopped: Bool { state.withLock { $0.stopped } }

    // MARK: running

    func execute(_ request: RunRequest, environment: [String: String]) async {
        let start = ContinuousClock.now
        let dir = options.tempRoot.appendingPathComponent("taisce-run", isDirectory: true).appendingPathComponent(UUID().uuidString, isDirectory: true)
        var result = await perform(request, environment: environment, dir: dir, start: start)
        // the temp dir goes before the run reports done: nothing outlives it
        try? FileManager.default.removeItem(at: dir)
        result.duration = ContinuousClock.now - start
        finish(result)
    }

    func perform(_ request: RunRequest, environment: [String: String], dir: URL, start: ContinuousClock.Instant) async -> RunResult {
        let deadline = start + options.timeout
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            var cwd = dir.path
            if let raw = request.cwd {
                let expanded = FenceInfo.expandTilde(raw, home: options.home)
                var isDir: ObjCBool = false
                guard FileManager.default.fileExists(atPath: expanded, isDirectory: &isDir), isDir.boolValue else {
                    return RunResult(message: RunMessages.noDirectory(raw))
                }
                cwd = expanded
            }
            var env = environment
            env["PWD"] = cwd
            switch request.language {
            case .shell(let interpreter):
                let script = dir.appendingPathComponent("block.sh")
                try Data(request.code.utf8).write(to: script)
                return await stream(SpawnSpec(executable: interpreter, arguments: [script.path], environment: env, directory: cwd), deadline: deadline, start: start)
            case .go:
                return try await runGo(request, env: env, dir: dir, cwd: cwd, deadline: deadline, start: start)
            }
        } catch {
            return RunResult(message: error.localizedDescription)
        }
    }

    func runGo(_ request: RunRequest, env: [String: String], dir: URL, cwd: String, deadline: ContinuousClock.Instant, start: ContinuousClock.Instant) async throws -> RunResult {
        guard let go = LoginEnvironment.which("go", path: env["PATH"]) else {
            return RunResult(message: RunMessages.noGo, goShape: GoProgram.classify(request.code))
        }
        var goEnv = env
        goEnv["GOWORK"] = "off"
        // a go.work or GOFLAGS=-mod=vendor elsewhere must not reach the temp module
        goEnv["GOFLAGS"] = nil
        continuation.yield(.phase("Building\u{2026}"))
        let version = try await capture(SpawnSpec(executable: go, arguments: ["env", "GOVERSION"], environment: goEnv, directory: dir.path), deadline: deadline)
        let prepared = GoProgram.prepare(request.code, tryLine: request.tryLine)
        try Data(GoProgram.goMod(goVersion: String(decoding: version.stdout, as: UTF8.self)).utf8).write(to: dir.appendingPathComponent("go.mod"))
        let main = dir.appendingPathComponent("main.go")
        var source = prepared.source
        try Data(source.utf8).write(to: main)
        if let goimports = LoginEnvironment.which("goimports", path: env["PATH"]) {
            _ = try? await capture(SpawnSpec(executable: goimports, arguments: ["-w", main.path], environment: goEnv, directory: dir.path), deadline: deadline)
        }
        // build; a wrapped block drops imports the compiler calls unused, then tries again
        var build = try await capture(SpawnSpec(executable: go, arguments: ["build", "-o", "prog", "."], environment: goEnv, directory: dir.path), deadline: deadline)
        var rounds = 0
        while build.status.exitCode != 0, prepared.shape != .program, rounds < 3, !build.timedOut, !isStopped {
            let unused = GoProgram.unusedImports(fromBuildOutput: String(decoding: build.stderr, as: UTF8.self))
            guard !unused.isEmpty else { break }
            source = GoProgram.removingImports(unused, from: (try? String(contentsOf: main, encoding: .utf8)) ?? source)
            try Data(source.utf8).write(to: main)
            build = try await capture(SpawnSpec(executable: go, arguments: ["build", "-o", "prog", "."], environment: goEnv, directory: dir.path), deadline: deadline)
            rounds += 1
        }
        if isStopped || build.timedOut || build.status.exitCode != 0 {
            let text = String(decoding: build.stderr + build.stdout, as: UTF8.self)
            emit(Data(Self.tidyBuildOutput(text, dir: dir.path).utf8), .stderr)
            return RunResult(
                exitCode: build.status.exitCode, signal: build.status.signal,
                timedOut: build.timedOut && !isStopped, stopped: isStopped, goShape: prepared.shape,
                buildFailed: !isStopped && !build.timedOut
            )
        }
        var r = await stream(SpawnSpec(executable: dir.appendingPathComponent("prog").path, environment: env, directory: cwd), deadline: deadline, start: start)
        r.goShape = prepared.shape
        return r
    }

    /// `./main.go:3:2: …` reads better than the temp dir's absolute path.
    static func tidyBuildOutput(_ s: String, dir: String) -> String {
        s.replacingOccurrences(of: dir + "/", with: "").replacingOccurrences(of: "# run\n", with: "")
    }

    func emit(_ data: Data, _ stream: OutputStreamKind) {
        let chunks = state.withLock { s in
            s.collector.add(data, from: stream)
            return s.collector.drain()
        }
        if !chunks.isEmpty { continuation.yield(.output(chunks)) }
    }

    /// A helper step (go env, build): raw output, bounded by the deadline,
    /// stoppable.
    func capture(_ spec: SpawnSpec, deadline: ContinuousClock.Instant) async throws -> ProcessCapture.Result {
        let remaining = max(.milliseconds(1), deadline - ContinuousClock.now)
        let run = self
        return try await ProcessCapture.run(spec, timeout: remaining, started: { p in
            let stopped = run.state.withLock { s in
                s.current = p
                return s.stopped
            }
            if stopped { p.terminateGroup() }
        })
    }

    /// The program itself: output streamed in batches, the deadline enforced.
    func stream(_ spec: SpawnSpec, deadline: ContinuousClock.Instant, start: ContinuousClock.Instant) async -> RunResult {
        continuation.yield(.phase("Running\u{2026}"))
        let run = self
        let timedOut = Mutex(false)
        let done = Mutex(false)
        let ticker = DispatchSource.makeTimerSource(queue: DispatchQueue(label: "taisce.run.batch"))
        let interval = options.batchInterval.dispatchInterval
        ticker.schedule(deadline: .now() + interval, repeating: interval)
        ticker.setEventHandler { run.flush() }
        ticker.resume()
        defer { ticker.cancel() }
        do {
            let status: ProcessStatus = try await withCheckedThrowingContinuation { cont in
                do {
                    let p = try SpawnedProcess(spec, onBytes: { kind, bytes in
                        run.state.withLock { $0.collector.add(bytes, from: kind) }
                    }, onExit: { st in
                        done.withLock { $0 = true }
                        cont.resume(returning: st)
                    })
                    let stopped = state.withLock { s in
                        s.current = p
                        return s.stopped
                    }
                    if stopped { p.terminateGroup() }
                    let remaining = max(.milliseconds(1), deadline - ContinuousClock.now)
                    DispatchQueue.global().asyncAfter(deadline: .now() + remaining.dispatchInterval) {
                        guard !done.withLock({ $0 }) else { return }
                        timedOut.withLock { $0 = true }
                        p.terminateGroup()
                    }
                } catch {
                    cont.resume(throwing: error)
                }
            }
            state.withLock { $0.collector.finish() }
            flush()
            let stopped = isStopped
            return RunResult(exitCode: status.exitCode, signal: status.signal, timedOut: timedOut.withLock { $0 } && !stopped, stopped: stopped)
        } catch {
            return RunResult(message: error.localizedDescription)
        }
    }

    func flush() {
        let (chunks, truncated) = state.withLock { s -> ([OutputChunk], Bool) in
            let c = s.collector.drain()
            let t = s.collector.truncated && !s.sentTruncated
            if t { s.sentTruncated = true }
            return (c, t)
        }
        if !chunks.isEmpty { continuation.yield(.output(chunks)) }
        if truncated { continuation.yield(.truncated) }
    }

    func finish(_ result: RunResult) {
        flush()
        continuation.yield(.finished(result))
        continuation.finish()
    }
}
#endif
