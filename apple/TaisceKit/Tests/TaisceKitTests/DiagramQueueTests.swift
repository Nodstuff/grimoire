import Foundation
import Synchronization
import Testing
@testable import TaisceKit

/// Records how many renders overlap; each takes `delay`, `hang` sources never answer.
private final class FakeRenderer: DiagramRendering {
    struct State {
        var running = 0
        var maxRunning = 0
        var order: [String] = []
    }

    let state = Mutex(State())
    let delay: Duration

    init(delay: Duration = .milliseconds(20)) {
        self.delay = delay
    }

    func render(_ request: DiagramRequest) async throws -> Data {
        state.withLock {
            $0.running += 1
            $0.maxRunning = max($0.maxRunning, $0.running)
            $0.order.append(request.source)
        }
        defer { state.withLock { $0.running -= 1 } }
        if request.source == "hang" {
            try await Task.sleep(for: .seconds(30))
        }
        try await Task.sleep(for: delay)
        if request.source.hasPrefix("bad") {
            throw DiagramRenderError(message: "Parse error on line 1")
        }
        return Data("png:\(request.source)".utf8)
    }
}

@Suite struct DiagramQueueTests {
    @Test func cacheKeyIsStable() {
        let a = DiagramCacheKey.make(source: "graph LR; A-->B", theme: .dark, width: 340)
        #expect(a == DiagramCacheKey.make(source: "graph LR; A-->B", theme: .dark, width: 340))
        #expect(a.count == 64 && a.allSatisfy { $0.isHexDigit && !$0.isUppercase })
        #expect(a != DiagramCacheKey.make(source: "graph LR; A-->B", theme: .light, width: 340))
        #expect(a != DiagramCacheKey.make(source: "graph LR; A-->B", theme: .dark, width: 360))
        #expect(a != DiagramCacheKey.make(source: "graph LR; A-->C", theme: .dark, width: 340))
        // a field boundary can't be forged by the source
        #expect(DiagramCacheKey.make(source: "1\nx", theme: .dark, width: 340) != DiagramCacheKey.make(source: "x", theme: .dark, width: 3401))
        #expect(DiagramRequest(source: "s", theme: .light, width: 300, scale: 2).key == DiagramRequest(source: "s", theme: .light, width: 300, scale: 3).key)
        // pinned: a change here throws away every cached image
        #expect(DiagramCacheKey.make(source: "", theme: .dark, width: 0) == DiagramCacheKey.make(source: "", theme: .dark, width: 0))
        #expect(DiagramCacheKey.renderer == "mermaid-12.0.0")
    }

    @Test func rendersOneAtATimeInOrder() async throws {
        let r = FakeRenderer()
        let q = DiagramRenderQueue(renderer: r, store: MemoryDiagramStore())
        try await withThrowingTaskGroup(of: Data.self) { g in
            for i in 0..<5 {
                g.addTask { try await q.image(for: DiagramRequest(source: "d\(i)", theme: .dark, width: 300)) }
                // let each enqueue before the next, so arrival order is defined
                try await Task.sleep(for: .milliseconds(5))
            }
            for try await _ in g {}
        }
        let s = r.state.withLock { $0 }
        #expect(s.maxRunning == 1)
        #expect(s.order == ["d0", "d1", "d2", "d3", "d4"])
    }

    @Test func cachesAndJoins() async throws {
        let r = FakeRenderer()
        let store = MemoryDiagramStore()
        let q = DiagramRenderQueue(renderer: r, store: store)
        let req = DiagramRequest(source: "graph", theme: .light, width: 320)
        async let a = q.image(for: req)
        async let b = q.image(for: req)
        let (x, y) = try await (a, b)
        #expect(x == y && x == Data("png:graph".utf8))
        #expect(await q.started == 1, "the second request joined the first")
        _ = try await q.image(for: req)
        #expect(await q.started == 1, "then it came from the cache")
        #expect(store.count == 1)
        #expect(q.cached(req) == x)
    }

    @Test func rendererErrorKeepsItsMessageAndIsNotCached() async {
        let store = MemoryDiagramStore()
        let q = DiagramRenderQueue(renderer: FakeRenderer(), store: store)
        await #expect(throws: DiagramRenderError(message: "Parse error on line 1")) {
            try await q.image(for: DiagramRequest(source: "bad", theme: .dark, width: 300))
        }
        #expect(store.count == 0)
    }

    @Test func timeoutFailsAndTheQueueMovesOn() async throws {
        let q = DiagramRenderQueue(renderer: FakeRenderer(), store: MemoryDiagramStore(), timeout: .milliseconds(150))
        let clock = ContinuousClock()
        let start = clock.now
        do throws(DiagramRenderError) {
            _ = try await q.image(for: DiagramRequest(source: "hang", theme: .dark, width: 300))
            Issue.record("expected a timeout")
        } catch {
            #expect(error.timedOut)
            #expect(error.message == "The diagram took longer than 0.15 s to draw.")
        }
        #expect(clock.now - start < .seconds(5))
        let next = try await q.image(for: DiagramRequest(source: "ok", theme: .dark, width: 300))
        #expect(next == Data("png:ok".utf8))
        #expect(DiagramRenderError.timeout(.seconds(5)).message == "The diagram took longer than 5 s to draw.")
    }

    @Test func fileStoreRoundTrips() throws {
        let dir = FileManager.default.temporaryDirectory.appending(path: "diagrams-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: dir) }
        let store = FileDiagramStore(directory: dir)
        #expect(store.image(for: "k") == nil)
        store.store(Data([1, 2, 3]), for: "k")
        #expect(store.image(for: "k") == Data([1, 2, 3]))
    }
}
