import SwiftUI
import TaisceKit
import UIKit

/// Draws a snapshot's visuals: Mermaid and reladraw as SVG from their own
/// offscreen web view (not the reading view's, whose queue renders PNGs at
/// the same time), charts as PNG through Swift Charts (`ImageRenderer`:
/// Swift Charts has no vector export). One draw at a time.
@MainActor
final class AppShareVisualRenderer: ShareVisualRendering {
    static let shared = AppShareVisualRenderer()
    /// the width a shared diagram is laid out at (pt): the page's column
    static let width = 720

    private lazy var web = DiagramWebView()
    private var tail: Task<Void, Never>?

    nonisolated func render(_ visual: ShareVisual) async throws -> RenderedVisual {
        try await serially { try await self.draw(visual) }
    }

    /// Runs `work` after every earlier draw has finished (not just timed out):
    /// two renders on one page would clear each other's host element.
    private func serially<T: Sendable>(_ work: @escaping @MainActor () async throws -> T) async throws -> T {
        let previous = tail
        let task = Task { @MainActor () -> Result<T, any Error> in
            await previous?.value
            do { return .success(try await work()) } catch { return .failure(error) }
        }
        tail = Task { _ = await task.value }
        return try await task.value.get()
    }

    private func draw(_ v: ShareVisual) async throws -> RenderedVisual {
        switch v.kind {
        case .mermaid, .reladraw:
            let request = DiagramRequest(kind: v.kind == .mermaid ? .mermaid : .reladraw, source: v.source, theme: v.theme, width: Self.width, scale: 1)
            let out = try await web.renderSVG(request)
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

extension AppModel {
    /// The doc as a share link (or PDF) would publish it: its cached blocks
    /// (with queued edits, as the reading view shows them), visuals drawn.
    func shareSnapshot(_ docID: DocID, theme: ShareSnapshot.Theme) async throws -> ShareSnapshotResult {
        guard let cache else { throw ShareLinkError.notAvailable }
        var blocks = try await cache.blocks(of: docID).map(\.block)
        if pendingWrites > 0, let editor = try? await cache.editor(for: docID) {
            blocks = editor.ordered().map(\.block)
        }
        if blocks.isEmpty, let sync {
            try await sync.refresh(docID)
            blocks = try await cache.blocks(of: docID).map(\.block)
        }
        let title = index.byID[docID]?.title ?? "Untitled"
        let builder = ShareSnapshotBuilder(renderer: AppShareVisualRenderer.shared, images: URLSessionShareImageLoader())
        return await builder.build(title: title, blocks: blocks, theme: theme)
    }
}

enum ShareLinkError: LocalizedError {
    case notAvailable

    var errorDescription: String? {
        switch self {
        case .notAvailable: "Share links need a signed-in Taisce server."
        }
    }
}

extension ShareSnapshot.Theme {
    /// The app's current appearance.
    init(_ scheme: ColorScheme) {
        self = scheme == .dark ? .dark : .light
    }
}
