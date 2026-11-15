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
            .background(Color.lapis.bg)
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
    /// - visuals drawn; linked images fetched only when `fetchImages`;
    /// - with `includeSubpages`, every doc under it too (the tree as it is
    ///   now, in sidebar order), each refreshed and read the same way.
    func shareSnapshot(_ docID: DocID, theme: ShareSnapshot.Theme, fetchImages: Bool, includeSubpages: Bool = false, purpose: SnapshotPurpose) async throws -> ShareSnapshotResult {
        guard let cache, let sync else { throw ShareLinkError.notAvailable }
        var subpages = includeSubpages ? index.descendants(of: docID) : []
        guard subpages.count <= ShareLimits.pages else { throw ShareLinkError.tooManyPages(subpages.count) }
        // the sites the person was asked about came from this copy: a refresh
        // that adds a new one (a collaborator, an agent) doesn't get it fetched
        let asked = fetchImages ? ImageQuestion.hosts(await linkedImageURLs(docID, includeSubpages: includeSubpages)) : []
        var stale: String?
        do {
            try await sync.refresh(docID)
        } catch {
            guard purpose == .pdf else { throw ShareLinkError.offline }
            stale = "Made from this device's copy (the server couldn't be reached): it may be out of date."
        }
        if !subpages.isEmpty {
            let gone = try await Self.refresh(subpages.map(\.id), sync: sync)
            // a page the server no longer has goes, with everything under it
            if !gone.isEmpty {
                var dropped = gone
                subpages = subpages.filter { page in
                    if dropped.contains(page.id) { return false }
                    if let p = page.parentID, dropped.contains(p) {
                        dropped.insert(page.id)
                        return false
                    }
                    return true
                }
            }
        }
        var (blocks, unsent) = try await cache.shareBlocks(for: docID)
        var pages: [SharePageSource] = []
        for page in subpages {
            let (pageBlocks, pageUnsent) = try await cache.shareBlocks(for: page.id)
            unsent = unsent || pageUnsent
            pages.append(SharePageSource(docID: page.id, parentDocID: page.parentID, title: page.title, blocks: pageBlocks))
        }
        try Task.checkCancellation()
        let title = index.byID[docID]?.title ?? "Untitled"
        let loader = SafeShareImageLoader()
        loader.allowedHosts = Set(asked)
        let builder = ShareSnapshotBuilder(renderer: AppShareVisualRenderer.shared, images: fetchImages ? loader : nil)
        var result = await builder.build(title: title, blocks: blocks, theme: theme, pages: pages)
        result.includesUnsentEdits = unsent
        if let stale { result.problems.insert(stale, at: 0) }
        return result
    }

    /// Sub-pages fetched from the server, a few at a time. A page the server
    /// no longer has (404: trashed since the tree was synced) comes back in
    /// the set; any other failure means the link can't be built now.
    private static func refresh(_ ids: [DocID], sync: SyncEngine) async throws -> Set<DocID> {
        try await withThrowingTaskGroup(of: DocID?.self) { group in
            var gone: Set<DocID> = []
            var next = ids.makeIterator()
            func add() {
                guard let id = next.next() else { return }
                group.addTask {
                    do {
                        try await sync.refresh(id)
                        return nil
                    } catch let e as APIError {
                        if case .notFound = e { return id }
                        throw ShareLinkError.offline
                    } catch is CancellationError {
                        throw CancellationError()
                    } catch {
                        throw ShareLinkError.offline
                    }
                }
            }
            for _ in 0..<4 { add() }
            while let result = try await group.next() {
                if let result { gone.insert(result) }
                add()
            }
            return gone
        }
    }

    /// The docs under `docID` a link with sub-pages would carry.
    func subpageCount(_ docID: DocID) -> Int {
        index.descendants(of: docID).count
    }

    /// The linked images a build would fetch, in order (from the cached
    /// docs): the doc's, and with `includeSubpages` every doc's under it.
    func linkedImageURLs(_ docID: DocID, includeSubpages: Bool = false) async -> [URL] {
        guard let cache, let (blocks, _) = try? await cache.shareBlocks(for: docID) else { return [] }
        var pages: [SharePageSource] = []
        if includeSubpages {
            for page in index.descendants(of: docID).prefix(ShareLimits.pages) {
                guard let (pageBlocks, _) = try? await cache.shareBlocks(for: page.id) else { continue }
                pages.append(SharePageSource(docID: page.id, parentDocID: page.parentID, title: page.title, blocks: pageBlocks))
            }
        }
        return ShareSnapshotBuilder.linkedImages(title: index.byID[docID]?.title ?? "", blocks: blocks, pages: pages)
    }

    /// How many linked images a build would fetch.
    func linkedImageCount(_ docID: DocID) async -> Int {
        await linkedImageURLs(docID).count
    }

    /// The hosts a build would fetch images from, in order, for the sheet's
    /// "Include 3 images from x.com, y.org?" (from the cached doc).
    func linkedImageHosts(_ docID: DocID) async -> [String] {
        ImageQuestion.hosts(await linkedImageURLs(docID))
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
    /// more docs under the doc than a link carries
    case tooManyPages(Int)

    var errorDescription: String? {
        switch self {
        case let .tooManyPages(n): "This doc has \(n) sub-pages; a link carries at most \(ShareLimits.pages). Share it without its sub-pages, or share a smaller part of the tree."
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
