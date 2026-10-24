import SwiftUI
import TaisceKit
import UIKit
import UniformTypeIdentifiers
import WebKit

/// What the reader page says (`apple/ReaderWeb/main.ts`, `post(type, …)`).
enum ReaderEvent: Equatable {
    struct TOCItem: Equatable, Identifiable, Hashable {
        let id: Int
        let label: String
        let href: String
        let depth: Int
    }

    struct Place: Equatable {
        var fraction: Double
        var cfi: String
        var xpointer: String
        var chapter: String
        var href: String?
        var minutes: Double?
    }

    case loaded
    case opened(title: String, toc: [TOCItem])
    case ready(exact: Bool)
    case relocate(Place)
    /// the reader turned a page or scrolled (their own move, not a jump)
    case moved
    case toggleChrome
    case failed(String)

    /// Parsed from a script message body; nil for anything unknown.
    init?(_ body: Any) {
        guard let m = body as? [String: Any], let type = m["type"] as? String else { return nil }
        switch type {
        case "loaded": self = .loaded
        case "opened":
            let raw = m["toc"] as? [[String: Any]] ?? []
            let toc = raw.enumerated().map { i, t in
                TOCItem(id: i, label: (t["label"] as? String) ?? "Untitled", href: (t["href"] as? String) ?? "", depth: (t["depth"] as? Int) ?? 0)
            }
            self = .opened(title: (m["title"] as? String) ?? "", toc: toc)
        case "ready": self = .ready(exact: (m["exact"] as? Bool) ?? true)
        case "relocate":
            guard let fraction = (m["fraction"] as? NSNumber)?.doubleValue else { return nil }
            self = .relocate(Place(fraction: fraction, cfi: (m["cfi"] as? String) ?? "", xpointer: (m["xpointer"] as? String) ?? "",
                                   chapter: (m["chapter"] as? String) ?? "", href: m["href"] as? String,
                                   minutes: (m["minutes"] as? NSNumber)?.doubleValue))
        case "moved": self = .moved
        case "toggleChrome": self = .toggleChrome
        case "error": self = .failed((m["message"] as? String) ?? "The book did not open.")
        default: return nil
        }
    }
}

/// Serves the reader page from the bundle (`reader/`), the app's
/// Newsreader, and the one book being read, at `taisce-reader://bundle/…`.
/// Nothing else: the page loads nothing from the network, and the book's
/// own scripts never run (the page's CSP and foliate both refuse them).
final class ReaderSchemeHandler: NSObject, WKURLSchemeHandler, @unchecked Sendable {
    static let scheme = "taisce-reader"
    static let pageURL = URL(string: "\(scheme)://bundle/reader.html")!
    static let bookURL = "\(scheme)://bundle/book.epub"

    private let lock = NSLock()
    private var _book: URL?

    /// The EPUB on disk that `book.epub` answers with.
    var book: URL? {
        get { lock.withLock { _book } }
        set { lock.withLock { _book = newValue } }
    }

    static func resource(_ path: String) -> (URL, String)? {
        switch path {
        case "/reader.html": Bundle.main.url(forResource: "reader", withExtension: "html", subdirectory: "reader").map { ($0, "text/html") }
        case "/reader.js": Bundle.main.url(forResource: "reader", withExtension: "js", subdirectory: "reader").map { ($0, "text/javascript") }
        case "/Newsreader-VF.ttf": Bundle.main.url(forResource: "Newsreader-VF", withExtension: "ttf").map { ($0, "font/ttf") }
        case "/Newsreader-Italic-VF.ttf": Bundle.main.url(forResource: "Newsreader-Italic-VF", withExtension: "ttf").map { ($0, "font/ttf") }
        default: nil
        }
    }

    func webView(_ webView: WKWebView, start task: any WKURLSchemeTask) {
        guard let url = task.request.url, url.host() == "bundle" else {
            task.didFailWithError(URLError(.unsupportedURL))
            return
        }
        let path = url.path()
        let found: (URL, String)? = path == "/book.epub" ? book.map { ($0, "application/epub+zip") } : Self.resource(path)
        guard let (file, type) = found, let data = try? Data(contentsOf: file, options: .mappedIfSafe) else {
            let r = HTTPURLResponse(url: url, statusCode: 404, httpVersion: "HTTP/1.1", headerFields: ["Content-Type": "text/plain"])!
            task.didReceive(r)
            task.didFinish()
            return
        }
        let headers = ["Content-Type": type, "Content-Length": String(data.count), "Cache-Control": "no-store"]
        task.didReceive(HTTPURLResponse(url: url, statusCode: 200, httpVersion: "HTTP/1.1", headerFields: headers)!)
        task.didReceive(data)
        task.didFinish()
    }

    func webView(_ webView: WKWebView, stop task: any WKURLSchemeTask) {}
}

/// The page and the native side's calls into it (`window.taisce`).
@MainActor
final class ReaderPage: NSObject, WKScriptMessageHandler, WKNavigationDelegate {
    let webView: WKWebView
    let schemes = ReaderSchemeHandler()
    var onEvent: (ReaderEvent) -> Void = { _ in }
    private var loaded = false
    private var queued: [String] = []

    override init() {
        let config = WKWebViewConfiguration()
        config.setURLSchemeHandler(schemes, forURLScheme: ReaderSchemeHandler.scheme)
        // nothing kept between books, and no cookies or storage to leak
        config.websiteDataStore = .nonPersistent()
        config.defaultWebpagePreferences.allowsContentJavaScript = true
        config.suppressesIncrementalRendering = true
        config.dataDetectorTypes = []
        let controller = WKUserContentController()
        config.userContentController = controller
        webView = WKWebView(frame: .zero, configuration: config)
        super.init()
        controller.add(WeakHandler(self), name: "reader")
        webView.navigationDelegate = self
        webView.isOpaque = false
        webView.backgroundColor = .clear
        webView.scrollView.backgroundColor = .clear
        webView.scrollView.isScrollEnabled = false
        webView.scrollView.contentInsetAdjustmentBehavior = .never
        webView.allowsLinkPreview = false
        #if DEBUG
        webView.isInspectable = true
        #endif
        webView.load(URLRequest(url: ReaderSchemeHandler.pageURL))
    }

    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage) {
        guard let e = ReaderEvent(message.body) else { return }
        if e == .loaded {
            loaded = true
            for js in queued { webView.evaluateJavaScript(js) }
            queued = []
        }
        onEvent(e)
    }

    /// Runs once the page's script is up (calls before that wait their turn).
    func call(_ js: String) {
        if loaded { webView.evaluateJavaScript(js) } else { queued.append(js) }
    }

    static func json(_ value: some Encodable) -> String {
        (try? String(data: JSONEncoder().encode(value), encoding: .utf8)) ?? "null"
    }

    func open(book: URL, start: ReaderSync.Start, look: ReaderLook) {
        schemes.book = book
        let s: String = switch start {
        case let .xpointer(xp, fraction, _): #"{"kind":"xpointer","xpointer":\#(Self.json(xp)),"fraction":\#(fraction)}"#
        case let .cfi(cfi, _): #"{"kind":"cfi","cfi":\#(Self.json(cfi))}"#
        case .start: #"{"kind":"start"}"#
        }
        call(#"window.taisce.open({url:\#(Self.json(ReaderSchemeHandler.bookURL)),start:\#(s),look:\#(Self.json(look))})"#)
    }

    func setLook(_ look: ReaderLook) { call("window.taisce.setLook(\(Self.json(look)))") }
    func next() { call("window.taisce.next()") }
    func prev() { call("window.taisce.prev()") }
    func goTo(href: String) { call("window.taisce.goTo(\(Self.json(href)))") }
    func goTo(fraction: Double) { call("window.taisce.goToFraction(\(fraction))") }
    func goTo(xpointer: String, fraction: Double) { call("window.taisce.goToXPointer(\(Self.json(xpointer)), \(fraction))") }

    /// Only the page itself and its blob: sections; a link out of the book
    /// opens in the system browser, never inside the reader.
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction) async -> WKNavigationActionPolicy {
        guard let url = action.request.url else { return .cancel }
        if url.scheme == ReaderSchemeHandler.scheme || url.scheme == "blob" || url.scheme == "about" { return .allow }
        if action.navigationType == .linkActivated, ["http", "https", "mailto"].contains(url.scheme ?? "") {
            await UIApplication.shared.open(url)
        }
        return .cancel
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        onEvent(.failed("The reader stopped. Close the book and open it again."))
    }
}

/// `WKUserContentController` keeps its handlers strongly: this breaks the cycle.
private final class WeakHandler: NSObject, WKScriptMessageHandler {
    weak var target: (any WKScriptMessageHandler)?
    init(_ target: any WKScriptMessageHandler) { self.target = target }
    func userContentController(_ c: WKUserContentController, didReceive message: WKScriptMessage) {
        target?.userContentController(c, didReceive: message)
    }
}

/// The page in SwiftUI.
struct ReaderWebView: UIViewRepresentable {
    let page: ReaderPage

    func makeUIView(context: Context) -> WKWebView { page.webView }
    func updateUIView(_ view: WKWebView, context: Context) {}
}
