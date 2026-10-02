#if os(macOS) || targetEnvironment(macCatalyst)
import Darwin
import Foundation
import Synchronization

/// What to start: an absolute executable, its arguments, the whole
/// environment, and the working directory.
public struct SpawnSpec: Sendable, Hashable {
    public var executable: String
    public var arguments: [String]
    public var environment: [String: String]
    public var directory: String?

    public init(executable: String, arguments: [String] = [], environment: [String: String], directory: String? = nil) {
        self.executable = executable
        self.arguments = arguments
        self.environment = environment
        self.directory = directory
    }
}

public enum SpawnError: Error, Sendable, Hashable, LocalizedError {
    case pipe(Int32)
    case spawn(Int32, String)

    public var errorDescription: String? {
        switch self {
        case .pipe(let e): "Couldn't create a pipe (\(String(cString: strerror(e))))"
        case .spawn(let e, let exe): "Couldn't start \(exe) (\(String(cString: strerror(e))))"
        }
    }
}

/// How a process ended: exit code or signal.
public struct ProcessStatus: Sendable, Hashable {
    public var exitCode: Int32?
    public var signal: Int32?

    init(waitStatus s: Int32) {
        if s & 0x7F == 0 {
            exitCode = (s >> 8) & 0xFF
            signal = nil
        } else {
            exitCode = nil
            signal = s & 0x7F
        }
    }

    public init(exitCode: Int32?, signal: Int32?) {
        self.exitCode = exitCode
        self.signal = signal
    }
}

/// One child process, started with `posix_spawn` (Foundation's `Process`
/// isn't available on Mac Catalyst) in its own process group, stdin on
/// /dev/null, stdout and stderr on pipes read by two background threads.
/// When the leader exits, anything left in its group (a backgrounded child
/// holding the pipes) is sent SIGTERM, then SIGKILL 2 s later, so a run
/// never leaves processes behind. `onBytes` and `onExit` are called off the
/// main actor; `onExit` once, after both pipes are closed.
public final class SpawnedProcess: Sendable {
    public let pid: pid_t
    let state = Mutex(State())

    struct State {
        var openPipes = 2
        var status: ProcessStatus?
        var exitReported = false
        var terminating = false
    }

    public static let killGrace: Duration = .seconds(2)

    public init(
        _ spec: SpawnSpec,
        onBytes: @escaping @Sendable (OutputStreamKind, [UInt8]) -> Void,
        onExit: @escaping @Sendable (ProcessStatus) -> Void
    ) throws {
        var outPipe: [Int32] = [-1, -1]
        var errPipe: [Int32] = [-1, -1]
        guard pipe(&outPipe) == 0 else { throw SpawnError.pipe(errno) }
        guard pipe(&errPipe) == 0 else {
            let e = errno
            close(outPipe[0]); close(outPipe[1])
            throw SpawnError.pipe(e)
        }
        // the parent's read ends must not leak into later children
        _ = fcntl(outPipe[0], F_SETFD, FD_CLOEXEC)
        _ = fcntl(errPipe[0], F_SETFD, FD_CLOEXEC)

        var actions: posix_spawn_file_actions_t?
        posix_spawn_file_actions_init(&actions)
        defer { posix_spawn_file_actions_destroy(&actions) }
        posix_spawn_file_actions_addopen(&actions, 0, "/dev/null", O_RDONLY, 0)
        posix_spawn_file_actions_adddup2(&actions, outPipe[1], 1)
        posix_spawn_file_actions_adddup2(&actions, errPipe[1], 2)

        var attr: posix_spawnattr_t?
        posix_spawnattr_init(&attr)
        defer { posix_spawnattr_destroy(&attr) }
        // own process group (pgid = its pid); only fds 0-2 survive; default
        // signal handling and an empty mask, whatever the app has set
        posix_spawnattr_setflags(&attr, Int16(POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT | POSIX_SPAWN_SETSIGDEF | POSIX_SPAWN_SETSIGMASK))
        posix_spawnattr_setpgroup(&attr, 0)
        var all = sigset_t()
        sigfillset(&all)
        posix_spawnattr_setsigdefault(&attr, &all)
        var none = sigset_t()
        sigemptyset(&none)
        posix_spawnattr_setsigmask(&attr, &none)

        // posix_spawn_file_actions_addchdir is macOS-only (not Catalyst):
        // a /bin/sh trampoline changes directory, then execs the program
        var argv: [String]
        let exe: String
        if let dir = spec.directory {
            exe = "/bin/sh"
            argv = ["sh", "-c", "cd -- \"$0\" && exec \"$@\"", dir, spec.executable] + spec.arguments
        } else {
            exe = spec.executable
            argv = [spec.executable] + spec.arguments
        }
        let env = spec.environment.map { "\($0.key)=\($0.value)" }.sorted()
        var cArgv: [UnsafeMutablePointer<CChar>?] = argv.map { strdup($0) } + [nil]
        var cEnv: [UnsafeMutablePointer<CChar>?] = env.map { strdup($0) } + [nil]
        defer {
            for p in cArgv { free(p) }
            for p in cEnv { free(p) }
        }
        var child: pid_t = 0
        let rc = posix_spawn(&child, exe, &actions, &attr, &cArgv, &cEnv)
        close(outPipe[1])
        close(errPipe[1])
        guard rc == 0 else {
            close(outPipe[0])
            close(errPipe[0])
            throw SpawnError.spawn(rc, spec.executable)
        }
        pid = child

        let readers: [(Int32, OutputStreamKind)] = [(outPipe[0], .stdout), (errPipe[0], .stderr)]
        for (fd, kind) in readers {
            let t = Thread { [self] in
                Self.drain(fd, kind, onBytes)
                close(fd)
                self.pipeClosed(onExit)
            }
            t.name = "taisce.run.\(kind)"
            t.start()
        }
        let leader = child
        let waiter = Thread { [self] in
            var status: Int32 = 0
            var r: pid_t
            repeat { r = waitpid(leader, &status, 0) } while r == -1 && errno == EINTR
            let st = ProcessStatus(waitStatus: r == leader ? status : 0)
            self.leaderExited(st, onExit)
        }
        waiter.name = "taisce.run.wait"
        waiter.start()
    }

    static func drain(_ fd: Int32, _ kind: OutputStreamKind, _ onBytes: @Sendable (OutputStreamKind, [UInt8]) -> Void) {
        var buf = [UInt8](repeating: 0, count: 64 * 1024)
        while true {
            let n = buf.withUnsafeMutableBytes { read(fd, $0.baseAddress, $0.count) }
            if n > 0 {
                onBytes(kind, Array(buf[0..<n]))
            } else if n == 0 || errno != EINTR {
                return
            }
        }
    }

    func pipeClosed(_ onExit: @Sendable (ProcessStatus) -> Void) {
        let report: ProcessStatus? = state.withLock { s in
            s.openPipes -= 1
            return reportIfDone(&s)
        }
        if let report { onExit(report) }
    }

    func leaderExited(_ status: ProcessStatus, _ onExit: @escaping @Sendable (ProcessStatus) -> Void) {
        let report: ProcessStatus? = state.withLock { s in
            s.status = status
            return reportIfDone(&s)
        }
        if let report {
            onExit(report)
            return
        }
        // the leader is gone but its pipes are still open: a backgrounded
        // child holds them. End the group so the run can finish.
        terminateGroup()
    }

    private func reportIfDone(_ s: inout State) -> ProcessStatus? {
        guard s.openPipes == 0, let st = s.status, !s.exitReported else { return nil }
        s.exitReported = true
        return st
    }

    /// Whether anything in the process group is still alive.
    public var groupAlive: Bool { kill(-pid, 0) == 0 }

    /// SIGTERM to the whole group, then SIGKILL after `killGrace` if any of
    /// it is still there. Safe to call more than once.
    public func terminateGroup() {
        let first = state.withLock { s in
            defer { s.terminating = true }
            return !s.terminating
        }
        guard first else { return }
        let pgid = pid
        kill(-pgid, SIGTERM)
        // a stopped process can't act on SIGTERM until it continues
        kill(-pgid, SIGCONT)
        let grace = Self.killGrace
        Thread.detachNewThread {
            let deadline = ContinuousClock.now + grace
            while ContinuousClock.now < deadline {
                if kill(-pgid, 0) != 0 { return }
                usleep(20_000)
            }
            kill(-pgid, SIGKILL)
        }
    }
}

/// Run something to completion and keep its raw output (the login-shell
/// capture, `go env`, a build): bounded by `timeout`, after which the
/// group is terminated.
public enum ProcessCapture {
    public struct Result: Sendable, Hashable {
        public var status: ProcessStatus
        public var stdout: Data
        public var stderr: Data
        public var timedOut: Bool
    }

    final class Box: Sendable {
        let state = Mutex<(out: Data, err: Data)>((Data(), Data()))
        let limit: Int
        init(limit: Int) { self.limit = limit }
    }

    public static func run(_ spec: SpawnSpec, timeout: Duration, limit: Int = 4 << 20, started: (@Sendable (SpawnedProcess) -> Void)? = nil) async throws -> Result {
        let box = Box(limit: limit)
        let flags = Mutex((done: false, timedOut: false))
        let status: ProcessStatus = try await withCheckedThrowingContinuation { cont in
            do {
                let p = try SpawnedProcess(spec, onBytes: { kind, bytes in
                    box.state.withLock { s in
                        if kind == .stdout {
                            if s.out.count < box.limit { s.out.append(contentsOf: bytes.prefix(box.limit - s.out.count)) }
                        } else if s.err.count < box.limit {
                            s.err.append(contentsOf: bytes.prefix(box.limit - s.err.count))
                        }
                    }
                }, onExit: { st in
                    flags.withLock { $0.done = true }
                    cont.resume(returning: st)
                })
                started?(p)
                DispatchQueue.global().asyncAfter(deadline: .now() + timeout.dispatchInterval) {
                    let late = flags.withLock { f in
                        if !f.done { f.timedOut = true }
                        return !f.done
                    }
                    if late { p.terminateGroup() }
                }
            } catch {
                cont.resume(throwing: error)
            }
        }
        let (out, err) = box.state.withLock { ($0.out, $0.err) }
        return Result(status: status, stdout: out, stderr: err, timedOut: flags.withLock { $0.timedOut })
    }
}

extension Duration {
    var dispatchInterval: DispatchTimeInterval {
        let (s, atto) = components
        let ns = s.multipliedReportingOverflow(by: 1_000_000_000)
        guard !ns.overflow else { return .never }
        return .nanoseconds(Int(ns.partialValue + atto / 1_000_000_000))
    }
}
#endif
