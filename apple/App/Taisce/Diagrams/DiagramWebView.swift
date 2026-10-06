import Foundation
import TaisceKit
import UIKit
import WebKit

/// The app's one diagram queue: Mermaid and reladraw drawn by the bundled
/// mermaid 12.0.0 and reladraw 0.15.1 in a shared offscreen web view, PNGs
/// cached in Caches/diagrams (`AppPaths.diagramCache`).
enum Diagrams {
    static let queue = DiagramRenderQueue(
        renderer: DiagramWebRenderer(),
        store: FileDiagramStore(directory: AppPaths.diagramCache)
    )
}

/// `DiagramRendering` over the main-actor web view.
struct DiagramWebRenderer: DiagramRendering {
    func render(_ request: DiagramRequest) async throws -> Data {
        try await DiagramWebView.shared.render(request)
    }
}

/// One non-persistent, offscreen WKWebView that never touches the network:
/// the page and its scripts come from the app bundle through the
/// `taisce-diagram:` scheme (reladraw ships as ES modules, which need a real
/// origin), a `default-src 'none'` CSP (scripts only from that origin, styles
/// inline for the SVG, images only as data: URLs), every navigation but the
/// first refused, mermaid's `securityLevel: 'strict'`. Mermaid or reladraw
/// renders SVG; the page rasterises it on a canvas at the device scale and
/// hands back PNG.
@MainActor
final class DiagramWebView: NSObject, WKNavigationDelegate {
    static let shared = DiagramWebView()
    static let scheme = "taisce-diagram"
    static let base = URL(string: "taisce-diagram://bundle/")!

    private var webView: WKWebView?
    private var loading: [CheckedContinuation<Void, any Error>] = []
    private var loaded = false

    static let csp = "default-src 'none'; script-src 'self'; style-src 'unsafe-inline'; img-src data:; font-src 'none'; connect-src 'none'; frame-src 'none'; object-src 'none'; base-uri 'none'; form-action 'none'"

    static var html: String {
        """
        <!doctype html>
        <html><head>
        <meta charset="utf-8">
        <meta http-equiv="Content-Security-Policy" content="\(csp)">
        <meta name="viewport" content="width=device-width, initial-scale=1">
        <style>html, body { margin: 0; background: transparent; } #host { position: absolute; left: 0; top: 0; }</style>
        <script src="mermaid.min.js"></script>
        </head><body><div id="host"></div></body></html>
        """
    }

    /// Runs in the page (callAsyncJavaScript: native calls aren't subject
    /// to the page's CSP, the page itself has no inline script).
    static let renderFunction = """
        const host = document.getElementById('host');
        host.innerHTML = '';
        host.style.width = width + 'px';
        let svg;
        if (kind === 'reladraw') {
          let rd;
          try {
            rd = await import(new URL('reladraw/index.js', document.baseURI).href);
          } catch (e) {
            return { error: 'The diagram renderer did not load: ' + String((e && e.message) || e) };
          }
          try {
            svg = rd.compile(source, { theme: rd.THEMES[theme === 'dark' ? 'dark' : 'light'] });
          } catch (e) {
            // SourceError.format() is "line 12: two placements for …"
            return { error: (e && e.name === 'SourceError' && e.format) ? e.format() : String((e && e.message) || e) };
          }
        } else {
        if (typeof mermaid === 'undefined') { return { error: 'The diagram renderer did not load.' }; }
        mermaid.initialize({
          startOnLoad: false, securityLevel: 'strict', theme: theme === 'dark' ? 'dark' : 'default',
          htmlLabels: false, flowchart: { htmlLabels: false },
          fontFamily: '-apple-system, system-ui, sans-serif'
        });
        window.__taisceSeq = (window.__taisceSeq || 0) + 1;
        try {
          ({ svg } = await mermaid.render('taisce' + window.__taisceSeq, source, host));
        } catch (e) {
          host.innerHTML = '';
          return { error: String((e && (e.message || e.str)) || e) };
        }
        }
        host.innerHTML = svg;
        const el = host.querySelector('svg');
        if (!el) { return { error: 'The renderer returned no SVG.' }; }
        const vb = el.viewBox && el.viewBox.baseVal;
        const box = el.getBoundingClientRect();
        let w = (vb && vb.width) || box.width, h = (vb && vb.height) || box.height;
        if (!(w > 0 && h > 0)) { return { error: 'The diagram is empty.' }; }
        if (w > width) { h = h * width / w; w = width; }
        el.setAttribute('width', String(w));
        el.setAttribute('height', String(h));
        el.style.maxWidth = 'none';
        const xml = new XMLSerializer().serializeToString(el);
        host.innerHTML = '';
        if (format === 'svg') { return { svg: xml, width: w, height: h }; }
        const img = new Image();
        img.src = 'data:image/svg+xml;charset=utf-8,' + encodeURIComponent(xml);
        await img.decode();
        let s = scale;
        const limit = 16e6;
        if (w * h * s * s > limit) { s = Math.sqrt(limit / (w * h)); }
        const canvas = document.createElement('canvas');
        canvas.width = Math.ceil(w * s);
        canvas.height = Math.ceil(h * s);
        const ctx = canvas.getContext('2d');
        ctx.scale(s, s);
        ctx.drawImage(img, 0, 0, w, h);
        try {
          return { png: canvas.toDataURL('image/png').split(',')[1] };
        } catch (e) {
          return { error: 'This diagram type uses HTML labels, which the phone cannot draw yet.' };
        }
        """

    func render(_ request: DiagramRequest) async throws -> Data {
        let web = try await ready()
        let result = try await web.callAsyncJavaScript(
            Self.renderFunction,
            arguments: Self.arguments(request, format: "png"),
            contentWorld: .page
        )
        guard let dict = result as? [String: Any] else { throw DiagramRenderError(message: "The diagram renderer gave no answer.") }
        if let error = dict["error"] as? String { throw DiagramRenderError(message: Self.tidy(error)) }
        guard let b64 = dict["png"] as? String, let data = Data(base64Encoded: b64), !data.isEmpty else {
            throw DiagramRenderError(message: "The diagram renderer returned no image.")
        }
        return data
    }

    /// The diagram as SVG markup (share links: drawn inside an `<img>`, so
    /// it can't run script), with its laid-out size in points.
    func renderSVG(_ request: DiagramRequest) async throws -> (svg: Data, width: Int, height: Int) {
        let web = try await ready()
        let result = try await web.callAsyncJavaScript(Self.renderFunction, arguments: Self.arguments(request, format: "svg"), contentWorld: .page)
        guard let dict = result as? [String: Any] else { throw DiagramRenderError(message: "The diagram renderer gave no answer.") }
        if let error = dict["error"] as? String { throw DiagramRenderError(message: Self.tidy(error)) }
        guard let svg = dict["svg"] as? String, !svg.isEmpty else { throw DiagramRenderError(message: "The diagram renderer returned no SVG.") }
        let w = (dict["width"] as? NSNumber)?.doubleValue ?? 0, h = (dict["height"] as? NSNumber)?.doubleValue ?? 0
        return (Data(Self.standalone(svg).utf8), Int(w.rounded(.up)), Int(h.rounded(.up)))
    }

    nonisolated static func arguments(_ request: DiagramRequest, format: String) -> [String: Any] {
        ["kind": request.kind.rawValue, "source": request.source, "theme": request.theme.rawValue, "width": request.width, "scale": request.scale, "format": format]
    }

    /// An SVG file needs its namespace (XMLSerializer adds it for an
    /// element from an HTML page only sometimes) and an XML prolog.
    nonisolated static func standalone(_ svg: String) -> String {
        var s = svg
        if !s.contains("xmlns=\"http://www.w3.org/2000/svg\""), let r = s.range(of: "<svg") {
            s.replaceSubrange(r, with: "<svg xmlns=\"http://www.w3.org/2000/svg\"")
        }
        return s.hasPrefix("<?xml") ? s : "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n" + s
    }

    /// Mermaid's parse errors carry a multi-line caret excerpt; keep it short
    /// (reladraw's are one line already: "line 2: edge to …").
    nonisolated static func tidy(_ message: String) -> String {
        let lines = message.split(separator: "\n").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        return lines.prefix(4).joined(separator: "\n")
    }

    private func ready() async throws -> WKWebView {
        if let webView, loaded { return webView }
        if webView == nil { start() }
        try await withCheckedThrowingContinuation { loading.append($0) }
        guard let webView else { throw DiagramRenderError(message: "The diagram renderer stopped.") }
        return webView
    }

    private func start() {
        let config = WKWebViewConfiguration()
        config.websiteDataStore = .nonPersistent()
        config.suppressesIncrementalRendering = true
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        config.preferences.javaScriptCanOpenWindowsAutomatically = false
        config.dataDetectorTypes = []
        config.setURLSchemeHandler(BundleSchemeHandler(), forURLScheme: Self.scheme)
        let web = WKWebView(frame: CGRect(x: 0, y: 0, width: 800, height: 600), configuration: config)
        web.navigationDelegate = self
        web.isOpaque = false
        webView = web
        loaded = false
        guard Bundle.main.url(forResource: "mermaid.min", withExtension: "js") != nil else {
            finish(DiagramRenderError(message: "The diagram renderer isn't in this build."))
            return
        }
        web.loadHTMLString(Self.html, baseURL: Self.base)
    }

    private func finish(_ error: (any Error)?) {
        let waiting = loading
        loading = []
        if let error {
            webView = nil
            loaded = false
            for c in waiting { c.resume(throwing: error) }
        } else {
            loaded = true
            for c in waiting { c.resume() }
        }
    }

    // MARK: WKNavigationDelegate

    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        // the page itself, loaded once; nothing else (links, redirects, frames)
        !loaded && action.targetFrame?.isMainFrame == true && action.navigationType == .other ? .allow : .cancel
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        finish(nil)
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: any Error) {
        finish(DiagramRenderError(message: error.localizedDescription))
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: any Error) {
        finish(DiagramRenderError(message: error.localizedDescription))
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        // the next render starts a fresh page
        self.webView = nil
        loaded = false
        finish(DiagramRenderError(message: "The diagram renderer stopped."))
    }
}

/// Serves `taisce-diagram://bundle/<path>` from the app bundle's resources:
/// scripts only, nothing outside the bundle, never the network.
final class BundleSchemeHandler: NSObject, WKURLSchemeHandler {
    static let types = ["js": "text/javascript", "mjs": "text/javascript"]

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        guard let url = task.request.url, let file = Self.file(for: url), let type = Self.types[file.pathExtension],
              let data = try? Data(contentsOf: file)
        else {
            task.didFailWithError(URLError(.fileDoesNotExist))
            return
        }
        let response = HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: [
            "Content-Type": "\(type); charset=utf-8",
            "Content-Length": String(data.count),
            "Cache-Control": "no-store",
        ])!
        task.didReceive(response)
        task.didReceive(data)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {}

    /// The bundle file a URL names, or nil (another host, `..`, missing).
    static func file(for url: URL) -> URL? {
        guard url.scheme == DiagramWebView.scheme, url.host() == "bundle", let root = Bundle.main.resourceURL?.standardizedFileURL else { return nil }
        let path = url.path(percentEncoded: false).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard !path.isEmpty, !path.split(separator: "/").contains("..") else { return nil }
        let file = root.appending(path: path).standardizedFileURL
        guard file.path.hasPrefix(root.path + "/"), FileManager.default.fileExists(atPath: file.path) else { return nil }
        return file
    }
}
