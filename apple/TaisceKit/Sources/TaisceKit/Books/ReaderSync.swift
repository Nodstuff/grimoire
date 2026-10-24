import Foundation

/// A kosync row as a device reads it (`reader.rs` `position_json`):
/// `progress` a KOReader XPointer, `percentage` the 0…1 fraction,
/// `timestamp` unix seconds on the server's clock.
public struct ReaderPosition: Codable, Sendable, Hashable {
    public var progress: String
    public var percentage: Double
    public var device: String
    public var deviceID: String
    public var timestamp: Int64

    public init(progress: String, percentage: Double, device: String, deviceID: String, timestamp: Int64) {
        self.progress = progress
        self.percentage = percentage
        self.device = device
        self.deviceID = deviceID
        self.timestamp = timestamp
    }

    enum CodingKeys: String, CodingKey {
        case progress, percentage, device, timestamp
        case deviceID = "device_id"
    }
}

/// What this device remembers per book: foliate's own CFI is finer than an
/// XPointer, so it wins when the last place was ours (`sync.ts` LocalPlace).
public struct LocalPlace: Codable, Sendable, Hashable {
    public var cfi: String
    public var fraction: Double
    /// the server stamp this place was written (or seen) at; 0 if never
    public var timestamp: Int64

    public init(cfi: String, fraction: Double, timestamp: Int64) {
        self.cfi = cfi
        self.fraction = fraction
        self.timestamp = timestamp
    }
}

/// `GET …/position`: the stored row (or none yet), or `unavailable` when
/// this server keeps no positions for us (LOCAL mode, an older server: 404).
public enum PositionRead: Sendable, Hashable {
    case available(ReaderPosition?)
    case unavailable
}

/// `PUT …/position`: written, refused because another device moved on
/// (409, with that row), or failed (retry with the next move).
public enum PositionWriteResult: Sendable, Hashable {
    case written(ReaderPosition)
    case newer(ReaderPosition)
    case failed
}

/// The body of a position write (`reader.rs` `PositionIn`).
public struct PositionWrite: Encodable, Sendable, Hashable {
    public var progress: String
    public var percentage: Double
    public var deviceID: String
    /// the timestamp of the row the reader last saw (0: none)
    public var base: Int64
    /// what other devices call this one ("Taisce on iPad"); nil: the
    /// server's "Taisce web" (servers before the field ignore it)
    public var device: String?

    public init(progress: String, percentage: Double, deviceID: String, base: Int64, device: String? = nil) {
        self.progress = progress
        self.percentage = percentage
        self.deviceID = deviceID
        self.base = base
        self.device = device
    }

    enum CodingKeys: String, CodingKey {
        case progress, percentage, base, device
        case deviceID = "device_id"
    }
}

/// The reader's place right now (from foliate's `relocate`).
public struct ReaderHere: Sendable, Hashable {
    public var progress: String
    public var percentage: Double

    public init(progress: String, percentage: Double) {
        self.progress = progress
        self.percentage = min(1, max(0, percentage))
    }
}

/// The web reader's sync decisions (`ui/src/reader/sync.ts`), the same on
/// every client so a phone, a browser and a Kindle agree.
public enum ReaderSync {
    public enum Start: Sendable, Hashable {
        case xpointer(String, fraction: Double, from: ReaderPosition)
        case cfi(String, fraction: Double)
        case start
    }

    /// Where to open: another device's newer place (by its XPointer, the
    /// percentage behind it), else this device's own CFI, else our own
    /// server row (another install of ours, or local state was cleared),
    /// else the beginning.
    public static func startTarget(remote: ReaderPosition?, local: LocalPlace?, mine: String) -> Start {
        if let remote, remote.deviceID != mine || local == nil, remote.timestamp > (local?.timestamp ?? 0) {
            return .xpointer(remote.progress, fraction: remote.percentage, from: remote)
        }
        if let local { return .cfi(local.cfi, fraction: local.fraction) }
        if let remote { return .xpointer(remote.progress, fraction: remote.percentage, from: remote) }
        return .start
    }

    /// The quiet note after a jump: only for another device's place.
    public static func syncToast(_ p: ReaderPosition, mine: String) -> String? {
        guard p.deviceID != mine else { return nil }
        let pct = Int((min(1, max(0, p.percentage)) * 100).rounded())
        return "Synced from \(p.device.isEmpty ? "another device" : p.device) · \(pct)%"
    }

    /// Has another device moved on since `seen` (the stamp we last saw)?
    public static func isNewer(_ remote: ReaderPosition?, seen: Int64, mine: String) -> Bool {
        guard let remote else { return false }
        return remote.deviceID != mine && remote.timestamp > seen
    }

    /// The XPointer to write when the converter found none: the section's body.
    public static func sectionXPointer(index: Int) -> String {
        "/body/DocFragment[\(index + 1)]/body"
    }
}

/// Debounced, guarded writes of the reader's place (`sync.ts`
/// PositionWriter): `moved` after each page turn (at most one write every
/// `minInterval`), `flush()` on background and close. A write carries
/// `base`, the stamp of the newest row seen; a 409 hands back the newer row
/// (`onNewer`), and the reader goes there instead of dragging the other
/// device back.
public actor PositionWriter {
    public typealias Put = @Sendable (PositionWrite) async -> PositionWriteResult

    private let device: String
    /// the name the rows carry ("Taisce on iPhone")
    private let name: String?
    private var seenStamp: Int64
    private let put: Put
    private let onNewer: @Sendable (ReaderPosition) async -> Void
    private let onWritten: @Sendable (ReaderPosition) async -> Void
    private let minInterval: Duration
    private let sleep: @Sendable (Duration) async throws -> Void
    private let now: @Sendable () -> ContinuousClock.Instant

    private var pending: ReaderHere?
    private var written: String?
    private var lastWrite: ContinuousClock.Instant?
    private var timer: Task<Void, Never>?
    private var inFlight: Task<PositionWriteResult, Never>?
    private var stopped = false

    public init(device: String, name: String? = nil, seen: Int64, minInterval: Duration = .seconds(10),
                sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) },
                now: @escaping @Sendable () -> ContinuousClock.Instant = { ContinuousClock.now },
                put: @escaping Put,
                onNewer: @escaping @Sendable (ReaderPosition) async -> Void,
                onWritten: @escaping @Sendable (ReaderPosition) async -> Void = { _ in }) {
        self.device = device
        self.name = name
        self.seenStamp = seen
        self.minInterval = minInterval
        self.sleep = sleep
        self.now = now
        self.put = put
        self.onNewer = onNewer
        self.onWritten = onWritten
    }

    /// The stamp of the newest row the reader has seen (a fetch on return).
    public func seen(_ stamp: Int64) {
        seenStamp = max(seenStamp, stamp)
    }

    public var base: Int64 { seenStamp }
    public var hasPending: Bool { pending != nil }

    /// The reader moved (a page turn, a jump, a scroll).
    public func moved(_ here: ReaderHere) {
        guard !stopped else { return }
        if here.progress == written, pending == nil { return }
        pending = here
        guard timer == nil else { return }
        let wait: Duration
        if let lastWrite {
            let next = lastWrite.advanced(by: minInterval)
            let gap = now().duration(to: next)
            wait = gap > .zero ? gap : .zero
        } else {
            wait = .zero
        }
        let sleep = self.sleep
        timer = Task { [weak self] in
            if wait > .zero { try? await sleep(wait) }
            guard !Task.isCancelled else { return }
            await self?.timerFired()
        }
    }

    private func timerFired() async {
        timer = nil
        await flush()
    }

    /// Write the pending place now (if any).
    public func flush() async {
        timer?.cancel()
        timer = nil
        if let inFlight { _ = await inFlight.value }
        guard let here = pending, !stopped else { return }
        pending = nil
        if here.progress == written { return }
        lastWrite = now()
        let body = PositionWrite(progress: here.progress, percentage: here.percentage, deviceID: device, base: seenStamp, device: name)
        let put = self.put
        let task = Task { await put(body) }
        inFlight = task
        let result = await task.value
        inFlight = nil
        switch result {
        case let .written(p):
            written = here.progress
            seenStamp = max(seenStamp, p.timestamp)
            await onWritten(p)
        case let .newer(p):
            seenStamp = max(seenStamp, p.timestamp)
            await onNewer(p)
        case .failed:
            // retried with the next move (or the next flush)
            if pending == nil { pending = here }
        }
    }

    /// Forget the unwritten place (another device's newer one is being taken).
    public func discard() {
        pending = nil
        timer?.cancel()
        timer = nil
    }

    public func stop() {
        stopped = true
        timer?.cancel()
        timer = nil
    }
}
