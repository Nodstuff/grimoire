import Foundation
import TaisceKit
import UIKit
import WebKit

/// The app's one diagram queue: Mermaid drawn by the bundled mermaid
/// 12.0.0 in a shared offscreen web view, PNGs cached in Caches/diagrams.
enum Diagrams {
    static let queue = DiagramRenderQueue(
        renderer: MermaidWebRenderer(),
        store: FileDiagramStore(directory: URL.cachesDirectory.appending(path: "diagrams", directoryHint: .isDirectory))
    )
}

/// `DiagramRendering` over the main-actor web view.
struct MermaidWebRenderer: DiagramRendering {
    func render(_ request: DiagramRequest) async throws -> Data {
        try await MermaidWebView.shared.render(request)
    }
}

/// One non-persistent, offscreen WKWebView that never touches the network:
/// a `default-src 'none'` CSP (scripts only from the app bundle, styles
/// inline for mermaid's SVG, images only as data: URLs), every navigation
/// but the first refused, `securityLevel: 'strict'`. Mermaid renders SVG;
/// the page rasterises it on a canvas at the device scale and hands back PNG.
@MainActor
final class MermaidWebView: NSObject, WKNavigationDelegate {
    static let shared = MermaidWebView()

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
        if (typeof mermaid === 'undefined') { return { error: 'The diagram renderer did not load.' }; }
        mermaid.initialize({
          startOnLoad: false, securityLevel: 'strict', theme: theme === 'dark' ? 'dark' : 'default',
          htmlLabels: false, flowchart: { htmlLabels: false },
          fontFamily: '-apple-system, system-ui, sans-serif'
        });
        const host = document.getElementById('host');
        host.innerHTML = '';
        host.style.width = width + 'px';
        window.__taisceSeq = (window.__taisceSeq || 0) + 1;
        let svg;
        try {
          ({ svg } = await mermaid.render('taisce' + window.__taisceSeq, source, host));
        } catch (e) {
          host.innerHTML = '';
          return { error: String((e && (e.message || e.str)) || e) };
        }
        host.innerHTML = svg;
        const el = host.querySelector('svg');
        if (!el) { return { error: 'Mermaid returned no SVG.' }; }
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
            arguments: ["source": request.source, "theme": request.theme.rawValue, "width": request.width, "scale": request.scale],
            contentWorld: .page
        )
        guard let dict = result as? [String: Any] else { throw DiagramRenderError(message: "The diagram renderer gave no answer.") }
        if let error = dict["error"] as? String { throw DiagramRenderError(message: Self.tidy(error)) }
        guard let b64 = dict["png"] as? String, let data = Data(base64Encoded: b64), !data.isEmpty else {
            throw DiagramRenderError(message: "The diagram renderer returned no image.")
        }
        return data
    }

    /// Mermaid's parse errors carry a multi-line caret excerpt; keep it short.
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
        let web = WKWebView(frame: CGRect(x: 0, y: 0, width: 800, height: 600), configuration: config)
        web.navigationDelegate = self
        web.isOpaque = false
        webView = web
        loaded = false
        guard let script = Bundle.main.url(forResource: "mermaid.min", withExtension: "js") else {
            finish(DiagramRenderError(message: "The diagram renderer isn't in this build."))
            return
        }
        web.loadHTMLString(Self.html, baseURL: script.deletingLastPathComponent())
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
