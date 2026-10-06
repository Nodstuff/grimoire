import Foundation

/// A visual block the app draws for a snapshot.
public struct ShareVisual: Sendable, Hashable {
    public enum Kind: String, Sendable, Hashable {
        case mermaid, reladraw
        case vegaLite = "vega-lite"

        public var label: String {
            switch self {
            case .mermaid: "Mermaid diagram"
            case .reladraw: "Reladraw diagram"
            case .vegaLite: "Chart"
            }
        }

        init?(language: String) {
            switch language {
            case "mermaid": self = .mermaid
            case "reladraw": self = .reladraw
            case "vega-lite", "vegalite": self = .vegaLite
            default: return nil
            }
        }
    }

    public var kind: Kind
    public var source: String
    public var theme: DiagramTheme
    /// the doc's tables, for charts that plot one (`{"block": "^abc123"}`)
    public var tables: ChartTables

    public init(kind: Kind, source: String, theme: DiagramTheme, tables: ChartTables = ChartTables()) {
        self.kind = kind
        self.source = source
        self.theme = theme
        self.tables = tables
    }
}

/// One drawn visual: SVG for diagrams, PNG where only an image exists (charts).
public struct RenderedVisual: Sendable, Hashable {
    public var data: Data
    public var contentType: String
    public var width: Int?
    public var height: Int?
    /// e.g. the chart's own title, for the alt text
    public var alt: String?

    public init(data: Data, contentType: String, width: Int? = nil, height: Int? = nil, alt: String? = nil) {
        self.data = data
        self.contentType = contentType
        self.width = width
        self.height = height
        self.alt = alt
    }

    public var fileExtension: String {
        switch contentType {
        case "image/svg+xml": "svg"
        case "image/jpeg": "jpg"
        case "image/webp": "webp"
        default: "png"
        }
    }
}

/// Draws visuals for a snapshot: the app's offscreen web view (diagrams)
/// and Swift Charts (charts); tests use fakes.
public protocol ShareVisualRendering: Sendable {
    func render(_ visual: ShareVisual) async throws -> RenderedVisual
}

/// Fetches an image a doc links to (`![alt](https://…)`).
public protocol ShareImageLoading: Sendable {
    func load(_ url: URL) async throws -> RenderedVisual
}

/// What came out of a build, and what didn't make it.
public struct ShareSnapshotResult: Sendable, Hashable {
    public var snapshot: ShareSnapshot
    /// one line per visual or image that failed or was left out
    public var problems: [String]

    public init(snapshot: ShareSnapshot, problems: [String] = []) {
        self.snapshot = snapshot
        self.problems = problems
    }
}

/// Turns a doc into the snapshot a share link publishes:
///
/// - blocks in document order, comments and deleted blocks left out;
/// - frontmatter stripped, and a leading H1 that repeats the title (the page shows the title);
/// - each mermaid / reladraw / vega-lite fence drawn by `renderer` and
///   replaced by `![alt](taisce-asset:dN.svg)`; every render waits at most
///   `timeout`, and a failure becomes a small SVG card saying so (never a gap);
/// - `![alt](https://…)` images fetched by `images` when they can be (else
///   the image becomes a plain link), within the server's limits.
public struct ShareSnapshotBuilder: Sendable {
    public var renderer: any ShareVisualRendering
    public var images: (any ShareImageLoading)?
    public var timeout: Duration
    public var imageTimeout: Duration

    public init(renderer: any ShareVisualRendering, images: (any ShareImageLoading)? = nil, timeout: Duration = .seconds(15), imageTimeout: Duration = .seconds(10)) {
        self.renderer = renderer
        self.images = images
        self.timeout = timeout
        self.imageTimeout = imageTimeout
    }

    public func build(title: String, blocks: [Block], theme: ShareSnapshot.Theme) async -> ShareSnapshotResult {
        let kept = blocks.filter { !$0.deleted && $0.blockType != .comment && $0.blockType != .canvasScene }
        let tables = ChartTables(blocks: kept.map { (id: $0.id, nodes: BlockRenderer.render($0)) })
        let diagramTheme: DiagramTheme = theme == .dark ? .dark : .light

        // 1. markdown with placeholders, and the work list
        var parts: [String] = []
        var pieces: [Piece] = []
        for (i, b) in kept.enumerated() {
            var content = b.content
            if i == 0 || parts.isEmpty {
                content = Self.strippingFrontmatter(content)
                content = Self.strippingTitleHeading(content, title: title)
            }
            if case .diagramMermaid = b.blockType {
                let body = BlockRenderer.fenceBody(content)
                pieces.append(.visual(ShareVisual(kind: .mermaid, source: body, theme: diagramTheme, tables: tables)))
                parts.append(Self.token(pieces.count - 1))
                continue
            }
            let (text, found) = Self.scan(content, theme: diagramTheme, tables: tables, startAt: pieces.count)
            pieces.append(contentsOf: found)
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { parts.append(text) }
        }
        var markdown = parts.joined(separator: "\n\n")

        // 2. draw / fetch, one at a time, each bounded
        var assets: [ShareAsset] = []
        var problems: [String] = []
        var total = markdown.utf8.count
        var diagramN = 0, imageN = 0
        for (index, piece) in pieces.enumerated() {
            let replacement: String
            switch piece {
            case let .visual(v):
                diagramN += 1
                let outcome = await draw(v)
                let base = "d\(diagramN)"
                switch outcome {
                case let .success(r):
                    let alt = r.alt.map { "\(v.kind.label): \($0)" } ?? v.kind.label
                    if let asset = Self.admit(r, name: "\(base).\(r.fileExtension)", assets: assets, total: total) {
                        assets.append(asset)
                        total += asset.data.count / 3 * 4
                        replacement = "![\(Self.escapeAlt(alt))](taisce-asset:\(asset.name))"
                    } else {
                        problems.append("\(alt) left out: too large to share")
                        replacement = "*(\(Self.escapeText(alt)) left out: too large to share)*"
                    }
                case let .failure(message):
                    problems.append("\(v.kind.label) couldn't render: \(message)")
                    let card = Self.errorCard(title: "\(v.kind.label) couldn't render", detail: message, theme: diagramTheme)
                    if let asset = Self.admit(card, name: "\(base)-error.svg", assets: assets, total: total) {
                        assets.append(asset)
                        total += asset.data.count / 3 * 4
                        replacement = "![\(Self.escapeAlt("\(v.kind.label) couldn't render: \(message)"))](taisce-asset:\(asset.name))"
                    } else {
                        replacement = "*(\(Self.escapeText(v.kind.label)) couldn't render: \(Self.escapeText(message)))*"
                    }
                }
            case let .image(alt, url, original):
                imageN += 1
                if let r = await fetch(url), let asset = Self.admit(r, name: "i\(imageN).\(r.fileExtension)", assets: assets, total: total) {
                    assets.append(asset)
                    total += asset.data.count / 3 * 4
                    replacement = "![\(alt)](taisce-asset:\(asset.name))"
                } else {
                    problems.append("Image \(url.absoluteString) couldn't be included")
                    // the share page only shows its own images: keep it reachable as a link
                    replacement = "[\(alt.isEmpty ? "Image" : alt)](\(original))"
                }
            }
            markdown = markdown.replacingOccurrences(of: Self.token(index), with: replacement)
        }
        return ShareSnapshotResult(snapshot: ShareSnapshot(title: title, markdown: markdown, assets: assets, theme: theme), problems: problems)
    }

    enum Piece: Sendable {
        case visual(ShareVisual)
        /// alt as written (already markdown), the URL, and the destination text as written
        case image(alt: String, url: URL, original: String)
    }

    enum Outcome { case success(RenderedVisual), failure(String) }

    func draw(_ v: ShareVisual) async -> Outcome {
        let renderer = renderer
        let limited = await withTimeLimit(timeout) { () async -> Result<RenderedVisual, ShareRenderFailure> in
            do {
                return .success(try await renderer.render(v))
            } catch {
                return .failure(ShareRenderFailure(message: (error as? LocalizedError)?.errorDescription ?? error.localizedDescription))
            }
        }
        switch limited {
        case let .finished(.success(r)):
            return r.data.isEmpty ? .failure("The renderer returned nothing.") : .success(r)
        case let .finished(.failure(f)):
            return .failure(f.message)
        case .timedOut:
            return .failure(DiagramRenderError.timeout(timeout).message)
        case let .threw(message):
            return .failure(message)
        }
    }

    func fetch(_ url: URL) async -> RenderedVisual? {
        guard let images else { return nil }
        if case let .finished(r) = await withTimeLimit(imageTimeout, { try await images.load(url) }) { return r }
        return nil
    }

    /// A placeholder no doc contains (U+E000 private use).
    static func token(_ i: Int) -> String { "\u{E000}taisce-share-\(i)\u{E000}" }

    /// The asset, if it fits the server's limits next to what's already in.
    static func admit(_ r: RenderedVisual, name: String, assets: [ShareAsset], total: Int) -> ShareAsset? {
        guard ShareAsset.allowedTypes.contains(r.contentType), ShareAsset.isValidName(name),
              r.data.count <= ShareLimits.assetBytes, assets.count < ShareLimits.assets,
              total + (r.data.count + 2) / 3 * 4 + 4096 <= ShareLimits.snapshotBytes
        else { return nil }
        return ShareAsset(name: name, contentType: r.contentType, data: r.data, width: r.width, height: r.height)
    }

    // MARK: markdown

    /// Fences of the visual languages become placeholders; `![](http…)`
    /// images outside fences too. Other fences are left alone.
    static func scan(_ md: String, theme: DiagramTheme, tables: ChartTables, startAt: Int) -> (String, [Piece]) {
        var out: [String] = []
        var pieces: [Piece] = []
        var lines = md.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false)[...]
        while let line = lines.popFirst() {
            let indent = line.prefix { $0 == " " }
            let t = line.dropFirst(indent.count)
            guard let fenceChar = t.first, fenceChar == "`" || fenceChar == "~", t.hasPrefix(String(repeating: fenceChar, count: 3)) else {
                out.append(images(in: String(line), pieces: &pieces, startAt: startAt))
                continue
            }
            let fence = t.prefix { $0 == fenceChar }
            let info = FenceInfo(String(t.dropFirst(fence.count)).trimmingCharacters(in: .whitespaces))
            var body: [Substring] = []
            var closed = false
            while let l = lines.popFirst() {
                let lt = l.drop { $0 == " " }
                if lt.hasPrefix(fence), lt.drop(while: { $0 == fenceChar }).allSatisfy(\.isWhitespace) {
                    closed = true
                    break
                }
                body.append(l)
            }
            if let lang = info.normalizedLanguage, let kind = ShareVisual.Kind(language: lang) {
                let source = body.map { $0.hasPrefix(indent) ? String($0.dropFirst(indent.count)) : String($0) }.joined(separator: "\n")
                pieces.append(.visual(ShareVisual(kind: kind, source: source, theme: theme, tables: tables)))
                out.append(String(indent) + token(startAt + pieces.count - 1))
            } else {
                out.append(String(line))
                out.append(contentsOf: body.map(String.init))
                if closed { out.append(String(indent) + String(fence)) }
            }
        }
        return (out.joined(separator: "\n"), pieces)
    }

    /// `![alt](http(s)://… "title")` → a placeholder. Inline code spans are skipped.
    static func images(in line: String, pieces: inout [Piece], startAt: Int) -> String {
        guard line.contains("![") else { return line }
        var result = ""
        var rest = Substring(line)
        while let open = rest.range(of: "![") {
            // inside a code span? (an odd number of backticks before it)
            let before = rest[..<open.lowerBound]
            result += before
            let ticks = (result.filter { $0 == "`" }).count
            guard ticks % 2 == 0, let closeAlt = rest[open.upperBound...].firstIndex(of: "]"),
                  rest.index(after: closeAlt) < rest.endIndex, rest[rest.index(after: closeAlt)] == "(",
                  let closeParen = rest[closeAlt...].firstIndex(of: ")")
            else {
                result += "!["
                rest = rest[open.upperBound...]
                continue
            }
            let alt = String(rest[open.upperBound..<closeAlt])
            let dest = String(rest[rest.index(closeAlt, offsetBy: 2)..<closeParen])
            var target = dest.trimmingCharacters(in: .whitespaces)
            if let space = target.firstIndex(of: " ") { target = String(target[..<space]) }
            target = target.trimmingCharacters(in: CharacterSet(charactersIn: "<>"))
            if let url = URL(string: target), let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http", url.host() != nil {
                pieces.append(.image(alt: alt, url: url, original: target))
                result += token(startAt + pieces.count - 1)
            } else {
                result += rest[open.lowerBound...closeParen]
            }
            rest = rest[rest.index(after: closeParen)...]
        }
        return result + rest
    }

    /// A leading `---` … `---` block goes; the rest of the block stays.
    static func strippingFrontmatter(_ s: String) -> String {
        let t = s.replacingOccurrences(of: "\r\n", with: "\n")
        guard t.hasPrefix("---\n") else { return s }
        let lines = t.split(separator: "\n", omittingEmptySubsequences: false)
        guard let end = lines.dropFirst().firstIndex(where: { $0.trimmingCharacters(in: .whitespaces) == "---" }) else { return s }
        return lines[(end + 1)...].joined(separator: "\n").trimmingCharacters(in: .newlines)
    }

    /// `# <title>` as the first line goes (the page has the title already).
    static func strippingTitleHeading(_ s: String, title: String) -> String {
        let lines = s.split(separator: "\n", omittingEmptySubsequences: false)
        guard let first = lines.first, first.hasPrefix("# "),
              first.dropFirst(2).trimmingCharacters(in: .whitespaces).caseInsensitiveCompare(title.trimmingCharacters(in: .whitespaces)) == .orderedSame
        else { return s }
        return lines.dropFirst().joined(separator: "\n").trimmingCharacters(in: .newlines)
    }

    static func escapeAlt(_ s: String) -> String {
        escapeText(s.replacingOccurrences(of: "\n", with: " "))
    }

    static func escapeText(_ s: String) -> String {
        var out = ""
        for ch in s {
            if "\\[]*_`<>".contains(ch) { out.append("\\") }
            out.append(ch == "\n" ? " " : ch)
        }
        return out
    }

    // MARK: error card

    /// A small SVG saying a visual didn't render, in the snapshot's theme.
    public static func errorCard(title: String, detail: String, theme: DiagramTheme) -> RenderedVisual {
        let dark = theme == .dark
        let bg = dark ? "#2a2520" : "#fff7ea"
        let border = dark ? "#8a6a3a" : "#e0b46a"
        let fg = dark ? "#f2e6d4" : "#4a3410"
        let sub = dark ? "#c9b79a" : "#7a5a26"
        let detailLines = wrap(detail.replacingOccurrences(of: "\n", with: " "), width: 90).prefix(3)
        let height = 44 + detailLines.count * 18
        var svg = """
        <svg xmlns="http://www.w3.org/2000/svg" width="640" height="\(height)" viewBox="0 0 640 \(height)">\
        <rect x="0.5" y="0.5" width="639" height="\(height - 1)" rx="8" fill="\(bg)" stroke="\(border)"/>\
        <text x="16" y="26" font-family="-apple-system, system-ui, sans-serif" font-size="14" font-weight="600" fill="\(fg)">\(xml("\u{26A0} " + title))</text>
        """
        for (i, line) in detailLines.enumerated() {
            svg += "<text x=\"16\" y=\"\(48 + i * 18)\" font-family=\"ui-monospace, Menlo, monospace\" font-size=\"12\" fill=\"\(sub)\">\(xml(line))</text>"
        }
        svg += "</svg>"
        return RenderedVisual(data: Data(svg.utf8), contentType: "image/svg+xml", width: 640, height: height)
    }

    static func wrap(_ s: String, width: Int) -> [String] {
        var lines: [String] = []
        var cur = ""
        for word in s.split(separator: " ") {
            if !cur.isEmpty, cur.count + 1 + word.count > width {
                lines.append(cur)
                cur = ""
            }
            cur += (cur.isEmpty ? "" : " ") + word.prefix(width)
        }
        if !cur.isEmpty { lines.append(cur) }
        return lines
    }

    static func xml(_ s: String) -> String {
        s.replacingOccurrences(of: "&", with: "&amp;").replacingOccurrences(of: "<", with: "&lt;")
            .replacingOccurrences(of: ">", with: "&gt;").replacingOccurrences(of: "\"", with: "&quot;")
    }
}

/// Fetches linked images over URLSession: http(s) only, 2 MB at most, and
/// only the types a share accepts (by header, else by magic bytes).
public struct URLSessionShareImageLoader: ShareImageLoading {
    let session: URLSession

    public init(session: URLSession = .shared) {
        self.session = session
    }

    public func load(_ url: URL) async throws -> RenderedVisual {
        guard let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http" else { throw URLError(.unsupportedURL) }
        var request = URLRequest(url: url, cachePolicy: .returnCacheDataElseLoad, timeoutInterval: 10)
        request.setValue("image/*", forHTTPHeaderField: "Accept")
        let (data, response) = try await session.data(for: request)
        guard let http = response as? HTTPURLResponse, (200..<300).contains(http.statusCode), !data.isEmpty,
              data.count <= ShareLimits.assetBytes
        else { throw URLError(.badServerResponse) }
        let declared = http.mimeType?.lowercased()
        guard let type = Self.sniff(data) ?? declared.flatMap({ ShareAsset.allowedTypes.contains($0) ? $0 : nil }) else {
            throw URLError(.cannotDecodeContentData)
        }
        let size = type == "image/png" ? Self.pngSize(data) : nil
        return RenderedVisual(data: data, contentType: type, width: size?.0, height: size?.1)
    }

    static func sniff(_ d: Data) -> String? {
        let b = [UInt8](d.prefix(16))
        if b.starts(with: [0x89, 0x50, 0x4E, 0x47]) { return "image/png" }
        if b.starts(with: [0xFF, 0xD8, 0xFF]) { return "image/jpeg" }
        if b.count >= 12, b.starts(with: Array("RIFF".utf8)), Array(b[8..<12]) == Array("WEBP".utf8) { return "image/webp" }
        let head = String(decoding: d.prefix(512), as: UTF8.self).trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if head.hasPrefix("<svg") || (head.hasPrefix("<?xml") && head.contains("<svg")) { return "image/svg+xml" }
        return nil
    }

    /// Width and height from a PNG's IHDR.
    static func pngSize(_ d: Data) -> (Int, Int)? {
        let b = [UInt8](d.prefix(24))
        guard b.count == 24 else { return nil }
        func be(_ i: Int) -> Int { Int(b[i]) << 24 | Int(b[i + 1]) << 16 | Int(b[i + 2]) << 8 | Int(b[i + 3]) }
        return (be(16), be(20))
    }
}

struct ShareRenderFailure: Error, Sendable {
    var message: String
}
