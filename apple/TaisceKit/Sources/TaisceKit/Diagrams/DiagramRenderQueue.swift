import CryptoKit
import Foundation
import Synchronization

public enum DiagramTheme: String, Sendable, Hashable {
    case dark, light
}

/// One diagram to rasterise: its source, the appearance and the width (pt)
/// it is laid out at. `scale` is the device's pixels per point.
public struct DiagramRequest: Sendable, Hashable {
    public var source: String
    public var theme: DiagramTheme
    public var width: Int
    public var scale: Double

    public init(source: String, theme: DiagramTheme, width: Int, scale: Double = 3) {
        self.source = source
        self.theme = theme
        self.width = width
        self.scale = scale
    }

    public var key: String { DiagramCacheKey.make(source: source, theme: theme, width: width) }
}

public enum DiagramCacheKey {
    /// bump when the bundled renderer changes, so old images are not reused
    public static let renderer = "mermaid-12.0.0"

    /// sha256 of renderer + theme + width + source, lowercase hex. Fields
    /// are newline-separated with the source last, so no two inputs share a key.
    public static func make(source: String, theme: DiagramTheme, width: Int) -> String {
        let input = "\(renderer)\n\(theme.rawValue)\n\(width)\n\(source)"
        return SHA256.hash(data: Data(input.utf8)).map { String(format: "%02x", $0) }.joined()
    }
}

/// A render that didn't produce an image: the renderer's own message
/// (mermaid's parse error), or a timeout.
public struct DiagramRenderError: Error, Sendable, Hashable, LocalizedError {
    public var message: String
    public var timedOut: Bool

    public init(message: String, timedOut: Bool = false) {
        self.message = message
        self.timedOut = timedOut
    }

    public static func timeout(_ limit: Duration) -> DiagramRenderError {
        let s = Double(limit.components.seconds) + Double(limit.components.attoseconds) / 1e18
        return DiagramRenderError(message: "The diagram took longer than \(ChartValue.format(s)) s to draw.", timedOut: true)
    }

    public var errorDescription: String? { message }
}

/// Draws one diagram to PNG data. The app's is a WKWebView running the
/// bundled mermaid; tests use fakes.
public protocol DiagramRendering: Sendable {
    func render(_ request: DiagramRequest) async throws -> Data
}

/// Rendered images by cache key.
public protocol DiagramImageStore: Sendable {
    func image(for key: String) -> Data?
    func store(_ data: Data, for key: String)
}

/// PNGs as files under a directory (the app uses Caches/diagrams): the
/// system may purge them, and a miss just renders again.
public final class FileDiagramStore: DiagramImageStore {
    public let directory: URL

    public init(directory: URL) {
        self.directory = directory
    }

    func url(_ key: String) -> URL { directory.appending(path: "\(key).png") }

    public func image(for key: String) -> Data? {
        try? Data(contentsOf: url(key))
    }

    public func store(_ data: Data, for key: String) {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try? data.write(to: url(key), options: .atomic)
    }
}

public final class MemoryDiagramStore: DiagramImageStore {
    private let images = Mutex<[String: Data]>([:])

    public init() {}

    public var count: Int { images.withLock { $0.count } }

    public func image(for key: String) -> Data? { images.withLock { $0[key] } }
    public func store(_ data: Data, for key: String) { images.withLock { $0[key] = data } }
}

/// Renders one diagram at a time through one renderer (one web view), in
/// arrival order: a cached image comes straight back, an identical request
/// already queued is joined rather than drawn twice, and each draw gets
/// `timeout` before it fails (the queue then moves on; a late answer is dropped).
public actor DiagramRenderQueue {
    public let renderer: any DiagramRendering
    public let store: any DiagramImageStore
    public let timeout: Duration

    private var tail: Task<Void, Never>?
    private var inflight: [String: Task<Result<Data, DiagramRenderError>, Never>] = [:]
    /// renders started (cache hits and joins excluded), for tests
    public private(set) var started = 0

    public init(renderer: any DiagramRendering, store: any DiagramImageStore, timeout: Duration = .seconds(5)) {
        self.renderer = renderer
        self.store = store
        self.timeout = timeout
    }

    /// The cached image, without queueing (nonisolated: a file read).
    public nonisolated func cached(_ request: DiagramRequest) -> Data? {
        store.image(for: request.key)
    }

    public func image(for request: DiagramRequest) async throws(DiagramRenderError) -> Data {
        let key = request.key
        if let hit = store.image(for: key) { return hit }
        if let running = inflight[key] { return try await running.value.get() }

        started += 1
        let previous = tail
        let renderer = renderer, store = store, timeout = timeout
        let task = Task<Result<Data, DiagramRenderError>, Never> {
            await previous?.value
            let outcome = await withTimeLimit(timeout) { () async -> Result<Data, DiagramRenderError> in
                do {
                    return .success(try await renderer.render(request))
                } catch let e as DiagramRenderError {
                    return .failure(e)
                } catch {
                    return .failure(DiagramRenderError(message: error.localizedDescription))
                }
            }
            switch outcome {
            case let .finished(result):
                if case let .success(data) = result { store.store(data, for: key) }
                return result
            case .timedOut:
                return .failure(.timeout(timeout))
            case let .threw(message):
                return .failure(DiagramRenderError(message: message))
            }
        }
        inflight[key] = task
        tail = Task { _ = await task.value }
        let result = await task.value
        inflight[key] = nil
        return try result.get()
    }
}
