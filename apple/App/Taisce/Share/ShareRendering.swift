import SwiftUI
import TaisceKit
import UIKit

/// Draws a snapshot's visuals: Mermaid and reladraw as SVG from their own
/// offscreen web view (not the reading view's, whose queue renders PNGs at
/// the same time), charts as PNG through Swift Charts (`ImageRenderer`:
/// Swift Charts has no vector export). One draw at a time, each bounded: a
/// draw that hangs past `timeout` throws, the web view it hung in is
/// dropped (the next draw starts a fresh one) and the queue moves on.
@MainActor
final class AppShareVisualRenderer: ShareVisualRendering {
    static let shared = AppShareVisualRenderer()
    /// the width a shared diagram is laid out at (pt): the page's column
    static let width = 720

    let timeout: Duration
    /// tests: a draw that stands in for the web view and charts
    var drawOverride: (@MainActor (ShareVisual) async throws -> RenderedVisual)?
    private(set) var web: DiagramWebView?
    private var tail: Task<Void, Never>?
    /// web views dropped after a hang (tests)
    private(set) var dropped = 0

    init(timeout: Duration = .seconds(12)) {
        self.timeout = timeout
    }

    nonisolated func render(_ visual: ShareVisual) async throws -> RenderedVisual {
        try await serially(visual)
    }

    /// Runs after every earlier draw has finished or timed out (two renders
    /// on one page would clear each other's host element).
    private func serially(_ visual: ShareVisual) async throws -> RenderedVisual {
        let previous = tail
        let timeout = timeout
        let task = Task { @MainActor [weak self] () -> Result<RenderedVisual, any Error> in
            await previous?.value
            guard let self else { return .failure(CancellationError()) }
            let outcome = await withTimeLimit(timeout) { @MainActor () async -> Result<RenderedVisual, DiagramRenderError> in
                do {
                    return .success(try await self.draw(visual))
                } catch let e as DiagramRenderError {
                    return .failure(e)
                } catch {
                    return .failure(DiagramRenderError(message: (error as? LocalizedError)?.errorDescription ?? error.localizedDescription))
                }
            }
            switch outcome {
            case let .finished(r): return r.mapError { $0 }
            case .timedOut:
                self.wedged()
                return .failure(DiagramRenderError.timeout(timeout))
            case let .threw(message): return .failure(DiagramRenderError(message: message))
            }
        }
        tail = Task { _ = await task.value }
        return try await task.value.get()
    }

    /// A draw hung: forget its web view (and the queue behind it).
    private func wedged() {
        web = nil
        tail = nil
        dropped += 1
    }

    private func draw(_ v: ShareVisual) async throws -> RenderedVisual {
        if let drawOverride { return try await drawOverride(v) }
        switch v.kind {
        case .mermaid, .reladraw:
            let request = DiagramRequest(kind: v.kind == .mermaid ? .mermaid : .reladraw, source: v.source, theme: v.theme, width: Self.width, scale: 1)
            let view = web ?? DiagramWebView()
            web = view
            let out = try await view.renderSVG(request)
            return RenderedVisual(data: out.svg, contentType: "image/svg+xml", width: out.width, height: out.height)
        case .vegaLite:
            return try Self.chartPNG(v)
        }
    }

    /// A vega-lite fence drawn as the reading view draws it, at 2x.
    static func chartPNG(_ v: ShareVisual) throws -> RenderedVisual {
        let model: ChartModel
        switch ChartSpec.parse(v.source, tables: v.tables) {
        case let .chart(m): model = m
        case let .unsupported(mark, reason): throw DiagramRenderError(message: "Chart type not supported yet (mark: \(mark), uses \(reason)).")
        case let .noData(mark, note): throw DiagramRenderError(message: "No data for this \(mark) chart: \(note)")
        case let .invalid(message, line, _): throw DiagramRenderError(message: line.map { "Line \($0): \(message)" } ?? message)
        }
        let scheme: ColorScheme = v.theme == .dark ? .dark : .light
        let content = ChartModelView(model: model)
            .frame(width: CGFloat(width))
            .background(Theme.ground)
            .environment(\.colorScheme, scheme)
        let renderer = ImageRenderer(content: content)
        renderer.scale = 2
        renderer.isOpaque = false
        guard let image = renderer.uiImage, let png = image.pngData() else {
            throw DiagramRenderError(message: "The chart couldn't be drawn to an image.")
        }
        return RenderedVisual(data: png, contentType: "image/png", width: Int(image.size.width), height: Int(image.size.height), alt: model.title)
    }
}

/// What a snapshot is for: a link must be built from the server's current
/// tree (refused offline); a PDF may use this device's copy.
enum SnapshotPurpose {
    case link, pdf
}

extension AppModel {
    /// The doc as a share link (or PDF) would publish it:
    /// - refreshed from the server first (a link refuses to build offline;
    ///   a PDF uses the cache, with a note);
    /// - the server's tree plus only this doc's pending writes (never refused
    ///   ones), flagged `includesUnsentEdits` when there were any;
    /// - visuals drawn; linked images fetched only when `fetchImages`.
    func shareSnapshot(_ docID: DocID, theme: ShareSnapshot.Theme, fetchImages: Bool, purpose: SnapshotPurpose) async throws -> ShareSnapshotResult {
        guard let cache, let sync else { throw ShareLinkError.notAvailable }
        var stale: String?
        do {
            try await sync.refresh(docID)
        } catch {
            guard purpose == .pdf else { throw ShareLinkError.offline }
            stale = "Made from this device's copy (the server couldn't be reached): it may be out of date."
        }
        let (blocks, unsent) = try await cache.shareBlocks(for: docID)
        let title = index.byID[docID]?.title ?? "Untitled"
        let builder = ShareSnapshotBuilder(renderer: AppShareVisualRenderer.shared, images: fetchImages ? SafeShareImageLoader() : nil)
        var result = await builder.build(title: title, blocks: blocks, theme: theme)
        result.includesUnsentEdits = unsent
        if let stale { result.problems.insert(stale, at: 0) }
        return result
    }

    /// How many linked images a build would fetch.
    func linkedImageCount(_ docID: DocID) async -> Int {
        guard let cache, let (blocks, _) = try? await cache.shareBlocks(for: docID) else { return 0 }
        return ShareSnapshotBuilder.linkedImages(title: index.byID[docID]?.title ?? "", blocks: blocks).count
    }

    /// The hosts a build would fetch images from, in order, for the sheet's
    /// "Include 3 images from x.com, y.org?" (from the cached doc).
    func linkedImageHosts(_ docID: DocID) async -> [String] {
        guard let cache, let (blocks, _) = try? await cache.shareBlocks(for: docID) else { return [] }
        let title = index.byID[docID]?.title ?? ""
        return ImageQuestion.hosts(ShareSnapshotBuilder.linkedImages(title: title, blocks: blocks))
    }
}

/// "Include 3 images from x.com, y.org?" before a build that would fetch them.
struct ImageQuestion: Identifiable, Equatable {
    let id = UUID()
    let count: Int
    let hosts: [String]

    static func hosts(_ urls: [URL]) -> [String] {
        var seen: Set<String> = []
        return urls.compactMap { $0.host()?.lowercased() }.filter { seen.insert($0).inserted }
    }

    init?(urls: [URL]) {
        guard !urls.isEmpty else { return nil }
        count = urls.count
        hosts = Self.hosts(urls)
    }

    init(count: Int, hosts: [String]) {
        self.count = count
        self.hosts = hosts
    }

    var title: String {
        let list = hosts.count <= 3 ? hosts.joined(separator: ", ") : hosts.prefix(3).joined(separator: ", ") + " and \(hosts.count - 3) more"
        return "Include \(count == 1 ? "1 image" : "\(count) images") from \(list)?"
    }

    static let message = "Taisce fetches them now and publishes copies with the link. Keep them as links to fetch nothing."
    static let include = "Include images"
    static let keepLinks = "Don't fetch, keep as links"
}

enum ShareLinkError: LocalizedError {
    case notAvailable
    case offline

    var errorDescription: String? {
        switch self {
        case .notAvailable: "Share links need a signed-in Taisce server."
        case .offline: "You're offline. A link is made from the doc as the server has it, so connect and try again."
        }
    }
}

extension ShareSnapshot.Theme {
    /// The app's current appearance.
    init(_ scheme: ColorScheme) {
        self = scheme == .dark ? .dark : .light
    }
}
