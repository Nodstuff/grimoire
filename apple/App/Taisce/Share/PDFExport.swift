import SwiftUI
import TaisceKit
import UIKit
import UniformTypeIdentifiers
import WebKit

/// Paper for PDF export: Letter where it's the norm, A4 elsewhere.
enum PaperSize: Equatable {
    case a4, letter

    static let letterRegions: Set<String> = ["US", "CA", "MX", "PH", "CL", "CO", "VE", "GT", "PR", "DO", "CR", "PA", "SV", "NI", "HN", "BO", "PE"]

    static func `for`(region: String?) -> PaperSize {
        region.map { letterRegions.contains($0.uppercased()) } == true ? .letter : .a4
    }

    static var current: PaperSize { .for(region: Locale.current.region?.identifier) }

    /// points
    var size: CGSize {
        switch self {
        case .a4: CGSize(width: 595.2, height: 841.8)
        case .letter: CGSize(width: 612, height: 792)
        }
    }

    static let margin: CGFloat = 40
}

/// How a PDF came out: paged for the paper, or one long page (the fallback
/// when the print formatter gives no pages).
enum PDFLayout: Equatable {
    case paged(pages: Int)
    case singlePage
}

/// The share page's HTML (from `POST /api/shares/preview`) in an offscreen,
/// script-free WKWebView, printed to PDF on A4 or Letter pages.
@MainActor
final class PDFRenderer: NSObject, WKNavigationDelegate {
    private var web: WKWebView?
    private var waiting: CheckedContinuation<Void, any Error>?
    private var loaded = false

    static func pdf(html: String, title: String, paper: PaperSize = .current, timeout: Duration = .seconds(30)) async throws -> (Data, PDFLayout) {
        let r = PDFRenderer()
        return try await r.render(html: html, title: title, paper: paper, timeout: timeout)
    }

    private func render(html: String, title: String, paper: PaperSize, timeout: Duration) async throws -> (Data, PDFLayout) {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.suppressesIncrementalRendering = true
        // the page's comment script is for readers: nothing runs here
        config.defaultWebpagePreferences.allowsContentJavaScript = false
        config.dataDetectorTypes = []
        // nothing but data: URLs loads (the page's own images are inlined)
        config.userContentController.add(try await Self.dataOnlyRules())
        let printable = paper.size.width - 2 * PaperSize.margin
        let web = WKWebView(frame: CGRect(x: 0, y: 0, width: printable, height: paper.size.height), configuration: config)
        web.navigationDelegate = self
        self.web = web
        defer { self.web = nil }

        let timer = Task { [weak self] in
            try? await Task.sleep(for: timeout)
            self?.finish(PDFExportError.timedOut)
        }
        defer { timer.cancel() }
        try await withCheckedThrowingContinuation { (c: CheckedContinuation<Void, any Error>) in
            waiting = c
            web.loadHTMLString(Self.lockedDown(html), baseURL: nil)
        }
        // every image loaded and decoded (lazy ones too), and the view as
        // tall as the page, so the print formatter lays out all of it
        let height = try await Self.settle(web)
        web.frame.size.height = min(max(height, paper.size.height), 400_000)
        web.layoutIfNeeded()

        if let paged = Self.paged(web, title: title, paper: paper) { return paged }
        let data = try await web.pdf(configuration: WKPDFConfiguration())
        return (data, .singlePage)
    }

    /// Runs as the app (`.defaultClient`), not the page: the page itself has
    /// JavaScript off. Images go eager and are awaited; returns the content height.
    static let settleScript = """
        const imgs = Array.from(document.images);
        for (const i of imgs) { i.loading = 'eager'; }
        await Promise.all(imgs.map(i => i.decode().catch(() => null)));
        const d = document.documentElement, b = document.body;
        return Math.ceil(Math.max(d.scrollHeight, d.offsetHeight, b ? b.scrollHeight : 0));
        """

    static func settle(_ web: WKWebView) async throws -> CGFloat {
        let h = try await web.callAsyncJavaScript(settleScript, arguments: [:], in: nil, contentWorld: .defaultClient)
        return CGFloat((h as? NSNumber)?.doubleValue ?? 0)
    }

    /// A CSP as the head's first element: images only from data:, inline
    /// styles, nothing else (no script, no fetches, no frames).
    static let csp = "default-src 'none'; img-src data:; style-src 'unsafe-inline'; font-src data:"

    static func lockedDown(_ html: String) -> String {
        let meta = "<meta http-equiv=\"Content-Security-Policy\" content=\"\(csp)\">"
        if let head = html.range(of: "<head[^>]*>", options: [.regularExpression, .caseInsensitive]) {
            return html.replacingCharacters(in: head, with: html[head] + meta)
        }
        if let doctype = html.range(of: "<!doctype[^>]*>", options: [.regularExpression, .caseInsensitive]) {
            return html.replacingCharacters(in: doctype, with: html[doctype] + "<head>" + meta + "</head>")
        }
        return "<head>" + meta + "</head>" + html
    }

    /// Blocks every load except data: URLs (and the about:blank document).
    static let ruleJSON = """
        [{"trigger":{"url-filter":".*"},"action":{"type":"block"}},
         {"trigger":{"url-filter":"^data:"},"action":{"type":"ignore-previous-rules"}},
         {"trigger":{"url-filter":"^about:"},"action":{"type":"ignore-previous-rules"}}]
        """

    private static var compiled: WKContentRuleList?

    static func dataOnlyRules() async throws -> WKContentRuleList {
        if let compiled { return compiled }
        guard let store = WKContentRuleListStore.default(),
              let list = try await store.compileContentRuleList(forIdentifier: "taisce-pdf-data-only", encodedContentRuleList: ruleJSON)
        else { throw PDFExportError.rendererStopped }
        compiled = list
        return list
    }

    /// UIPrintPageRenderer over the web view's print formatter: real page breaks.
    static func paged(_ web: WKWebView, title: String, paper: PaperSize) -> (Data, PDFLayout)? {
        let renderer = UIPrintPageRenderer()
        renderer.addPrintFormatter(web.viewPrintFormatter(), startingAtPageAt: 0)
        let paperRect = CGRect(origin: .zero, size: paper.size)
        let printableRect = paperRect.insetBy(dx: PaperSize.margin, dy: PaperSize.margin)
        // UIPrintPageRenderer reads these; set outside a print job they size the pages
        renderer.setValue(NSValue(cgRect: paperRect), forKey: "paperRect")
        renderer.setValue(NSValue(cgRect: printableRect), forKey: "printableRect")
        let pages = renderer.numberOfPages
        guard pages > 0 else { return nil }
        let format = UIGraphicsPDFRendererFormat()
        format.documentInfo = [kCGPDFContextTitle as String: title, kCGPDFContextCreator as String: "Taisce"]
        let data = UIGraphicsPDFRenderer(bounds: paperRect, format: format).pdfData { ctx in
            renderer.prepare(forDrawingPages: NSRange(location: 0, length: pages))
            for i in 0..<pages {
                ctx.beginPage()
                renderer.drawPage(at: i, in: ctx.pdfContextBounds)
            }
        }
        return (data, .paged(pages: pages))
    }

    private func finish(_ error: (any Error)?) {
        guard let c = waiting else { return }
        waiting = nil
        if let error { c.resume(throwing: error) } else { c.resume() }
    }

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        // the page itself, once; no links, redirects or frames
        if !loaded, action.targetFrame?.isMainFrame == true {
            loaded = true
            return .allow
        }
        return .cancel
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) { finish(nil) }
    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error) { finish(error) }
    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: any Error) { finish(error) }
    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) { finish(PDFExportError.rendererStopped) }
}

enum PDFExportError: LocalizedError {
    case timedOut, rendererStopped

    var errorDescription: String? {
        switch self {
        case .timedOut: "The PDF took too long to lay out."
        case .rendererStopped: "The PDF renderer stopped."
        }
    }
}

/// A finished export: a file named after the doc, in a temporary folder of its own.
struct PDFExportFile: Identifiable, Equatable {
    let url: URL
    let layout: PDFLayout
    /// what didn't make it, or that it came from this device's copy
    var notes: [String] = []
    var id: URL { url }

    static var root: URL { FileManager.default.temporaryDirectory.appending(path: "pdf-export", directoryHint: .isDirectory) }

    /// The export's own folder goes once the file was saved or shared.
    func cleanUp() {
        let dir = url.deletingLastPathComponent()
        guard dir.deletingLastPathComponent().standardizedFileURL == Self.root.standardizedFileURL else { return }
        try? FileManager.default.removeItem(at: dir)
    }

    /// At launch: whatever an earlier run left behind.
    static func sweep(root: URL = PDFExportFile.root) {
        try? FileManager.default.removeItem(at: root)
    }

    /// The doc title as a file name: no path separators or colons, not empty, not too long.
    static func filename(_ title: String) -> String {
        var name = title.replacingOccurrences(of: "/", with: "-").replacingOccurrences(of: ":", with: "-")
            .replacingOccurrences(of: "\\", with: "-")
        name = String(name.unicodeScalars.filter { !CharacterSet.controlCharacters.contains($0) })
        name = name.trimmingCharacters(in: .whitespacesAndNewlines.union(CharacterSet(charactersIn: ".")))
        if name.isEmpty { name = "Untitled" }
        return String(name.prefix(120)) + ".pdf"
    }
}

extension AppModel {
    /// The doc as PDF: a light-theme snapshot (paper; this device's copy if
    /// the server can't be reached, with a note), the server's page for it,
    /// printed to pages. The file sits in a folder of its own under
    /// tmp/pdf-export until `PDFExportFile.cleanUp`.
    func exportPDF(_ docID: DocID, fetchImages: Bool) async throws -> PDFExportFile {
        guard authPhase == .signedIn, let api else { throw ShareLinkError.notAvailable }
        let built = try await shareSnapshot(docID, theme: .light, fetchImages: fetchImages, purpose: .pdf)
        let html = try await api.sharePreviewHTML(built.snapshot)
        let (data, layout) = try await PDFRenderer.pdf(html: html, title: built.snapshot.title)
        let dir = PDFExportFile.root.appending(path: UUID().uuidString, directoryHint: .isDirectory)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appending(path: PDFExportFile.filename(built.snapshot.title))
        try data.write(to: url, options: .atomic)
        return PDFExportFile(url: url, layout: layout, notes: built.problems)
    }
}

/// The file for the Mac's save panel (`fileExporter`).
struct PDFDocumentFile: FileDocument {
    static let readableContentTypes: [UTType] = [.pdf]
    var data: Data

    init(data: Data) { self.data = data }

    init(configuration: ReadConfiguration) throws {
        data = configuration.file.regularFileContents ?? Data()
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        FileWrapper(regularFileWithContents: data)
    }
}

/// The iPhone's share sheet for a file.
struct ActivityView: UIViewControllerRepresentable {
    let items: [Any]

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: items, applicationActivities: nil)
    }

    func updateUIViewController(_ controller: UIActivityViewController, context: Context) {}
}
