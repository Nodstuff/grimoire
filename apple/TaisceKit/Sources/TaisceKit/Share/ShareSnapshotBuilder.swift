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
    /// the blocks included edits still waiting in this device's outbox
    public var includesUnsentEdits: Bool

    public init(snapshot: ShareSnapshot, problems: [String] = [], includesUnsentEdits: Bool = false) {
        self.snapshot = snapshot
        self.problems = problems
        self.includesUnsentEdits = includesUnsentEdits
    }
}

/// Turns a doc into the snapshot a share link publishes:
///
/// - blocks in document order, of the published types only (`publishedTypes`;
///   comments, canvases and anything unknown stay out);
/// - frontmatter stripped, and a leading H1 that repeats the title (the page shows the title);
/// - each mermaid / reladraw / vega-lite fence drawn by `renderer` and
///   replaced by `![alt](taisce-asset:dN.svg)`; every render waits at most
///   `timeout`, and a failure becomes a small SVG card saying so (never a gap);
/// - `![alt](https://…)` images fetched by `images` (`SafeShareImageLoader`)
///   when given and allowed, else kept as a plain link; within the server's limits.
public struct ShareSnapshotBuilder: Sendable {
    public var renderer: any ShareVisualRendering
    /// nil: linked images stay links (the person chose not to fetch them)
    public var images: (any ShareImageLoading)?
    public var timeout: Duration
    public var imageTimeout: Duration

    /// The block types a link publishes. Everything else is left out.
    public static let publishedTypes: Set<String> = [
        BlockType.paragraph.rawValue, BlockType.heading.rawValue, BlockType.code.rawValue,
        BlockType.diagramMermaid.rawValue, BlockType.diagramD2.rawValue, BlockType.decision.rawValue,
    ]

    public init(renderer: any ShareVisualRendering, images: (any ShareImageLoading)? = nil, timeout: Duration = .seconds(15), imageTimeout: Duration = .seconds(10)) {
        self.renderer = renderer
        self.images = images
        self.timeout = timeout
        self.imageTimeout = imageTimeout
    }

    /// The linked images a build would fetch (http and https), in order: the
    /// share sheet asks about their hosts first.
    public static func linkedImages(title: String, blocks: [Block]) -> [URL] {
        prepare(title: title, blocks: blocks, theme: .light).pieces.compactMap {
            if case let .image(_, url, _) = $0 { return url }
            return nil
        }
    }

    public func build(title: String, blocks: [Block], theme: ShareSnapshot.Theme) async -> ShareSnapshotResult {
        let diagramTheme: DiagramTheme = theme == .dark ? .dark : .light
        let prepared = Self.prepare(title: title, blocks: blocks, theme: diagramTheme)
        var markdown = prepared.markdown
        var problems = prepared.problems

        // draw / fetch, one at a time, each bounded
        var assets: [ShareAsset] = []
        var total = markdown.utf8.count
        var diagramN = 0, imageN = 0
        for (index, piece) in prepared.pieces.enumerated() {
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
                // the share page only shows its own images: anything not fetched stays a link
                let link = "[\(alt.isEmpty ? "Image" : alt)](\(original))"
                guard images != nil else {
                    replacement = link
                    break
                }
                imageN += 1
                switch await fetch(url) {
                case let .success(r):
                    if let asset = Self.admit(r, name: "i\(imageN).\(r.fileExtension)", assets: assets, total: total) {
                        assets.append(asset)
                        total += asset.data.count / 3 * 4
                        replacement = "![\(alt)](taisce-asset:\(asset.name))"
                    } else {
                        problems.append("Image from \(url.host() ?? "?") kept as a link: too large to share")
                        replacement = link
                    }
                case let .failure(why):
                    problems.append("Image from \(url.host() ?? "?") kept as a link: \(why.message)")
                    replacement = link
                }
            }
            markdown = markdown.replacingOccurrences(of: Self.token(index), with: replacement)
        }
        return ShareSnapshotResult(snapshot: ShareSnapshot(title: title, markdown: markdown, assets: assets, theme: theme), problems: problems)
    }

    struct Prepared {
        var markdown: String
        var pieces: [Piece]
        var problems: [String]
    }

    /// The markdown with placeholders, the work list, and what was left out.
    static func prepare(title: String, blocks: [Block], theme: DiagramTheme) -> Prepared {
        var problems: [String] = []
        let live = blocks.filter { !$0.deleted }
        let kept = live.filter { publishedTypes.contains($0.blockType.rawValue) }
        let skipped = Set(live.map(\.blockType.rawValue)).subtracting(publishedTypes).subtracting([BlockType.comment.rawValue])
        for type in skipped.sorted() {
            problems.append(type == BlockType.canvasScene.rawValue ? "Canvases aren't shared" : "A block of type \u{201C}\(type)\u{201D} was left out")
        }
        let tables = ChartTables(blocks: kept.map { (id: $0.id, nodes: BlockRenderer.render($0)) })
        var parts: [String] = []
        var pieces: [Piece] = []
        for b in kept {
            var content = b.content
            if parts.isEmpty {
                content = strippingFrontmatter(content)
                content = strippingTitleHeading(content, title: title)
            }
            if case .diagramMermaid = b.blockType {
                pieces.append(.visual(ShareVisual(kind: .mermaid, source: BlockRenderer.fenceBody(content), theme: theme, tables: tables)))
                parts.append(token(pieces.count - 1))
                continue
            }
            let (text, found) = scan(content, theme: theme, tables: tables, startAt: pieces.count)
            pieces.append(contentsOf: found)
            if !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty { parts.append(text) }
        }
        return Prepared(markdown: parts.joined(separator: "\n\n"), pieces: pieces, problems: problems)
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

    func fetch(_ url: URL) async -> Result<RenderedVisual, ShareRenderFailure> {
        guard let images else { return .failure(ShareRenderFailure(message: "not fetched")) }
        let limited = await withTimeLimit(imageTimeout) { () async -> Result<RenderedVisual, ShareRenderFailure> in
            do {
                return .success(try await images.load(url))
            } catch {
                return .failure(ShareRenderFailure(message: (error as? LocalizedError)?.errorDescription ?? error.localizedDescription))
            }
        }
        switch limited {
        case let .finished(r): return r
        case .timedOut: return .failure(ShareRenderFailure(message: "it took too long"))
        case let .threw(m): return .failure(ShareRenderFailure(message: m))
        }
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

    /// A line split into its container prefix (indentation and blockquote
    /// markers) and the rest.
    static func split(_ line: Substring) -> (prefix: Substring, rest: Substring) {
        var i = line.startIndex
        while i < line.endIndex {
            let c = line[i]
            if c == " " || c == "\t" {
                i = line.index(after: i)
            } else if c == ">" {
                i = line.index(after: i)
            } else {
                break
            }
        }
        return (line[..<i], line[i...])
    }

    /// Indentation width, tabs to the next multiple of 4.
    static func width(_ s: Substring) -> Int {
        s.reduce(0) { w, c in c == "\t" ? (w / 4 + 1) * 4 : w + 1 }
    }

    static func isListItem(_ rest: Substring) -> Bool {
        if let f = rest.first, "-*+".contains(f) {
            return rest.dropFirst().first.map { $0 == " " || $0 == "\t" } ?? true
        }
        let digits = rest.prefix { $0.isASCII && $0.isNumber }
        guard !digits.isEmpty, digits.count <= 9 else { return false }
        let after = rest.dropFirst(digits.count)
        guard let d = after.first, d == "." || d == ")" else { return false }
        return after.dropFirst().first.map { $0 == " " || $0 == "\t" } ?? true
    }

    /// Fences of the visual languages become placeholders, also inside
    /// blockquotes and (tab- or space-) indented lists; `![](http…)` images
    /// outside code too. Other fences and indented code blocks are left alone.
    static func scan(_ md: String, theme: DiagramTheme, tables: ChartTables, startAt: Int) -> (String, [Piece]) {
        var out: [String] = []
        var pieces: [Piece] = []
        var lines = md.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false)[...]
        var inList = false
        var prevBlank = true
        var inIndentedCode = false
        while let line = lines.popFirst() {
            let (prefix, rest) = split(line)
            let quoted = prefix.contains(">")
            if rest.isEmpty {
                out.append(String(line))
                prevBlank = true
                continue
            }
            let indent = width(prefix)
            // 4-space (or tab) indented code, outside lists and quotes: verbatim
            if !quoted, !inList, indent >= 4, prevBlank || inIndentedCode {
                out.append(String(line))
                inIndentedCode = true
                prevBlank = false
                continue
            }
            inIndentedCode = false
            if !quoted, indent < 4, isListItem(rest) {
                inList = true
            } else if !quoted, indent == 0, prevBlank {
                inList = false
            }
            prevBlank = false

            guard let fenceChar = rest.first, fenceChar == "`" || fenceChar == "~", rest.hasPrefix(String(repeating: fenceChar, count: 3)) else {
                out.append(String(prefix) + images(in: String(rest), pieces: &pieces, startAt: startAt))
                continue
            }
            let fence = rest.prefix { $0 == fenceChar }
            let info = FenceInfo(String(rest.dropFirst(fence.count)).trimmingCharacters(in: .whitespaces))
            var body: [Substring] = []
            var raw: [Substring] = []
            var closed = false
            while let l = lines.first {
                // a quoted fence ends with its quote
                let inner: Substring
                if l.hasPrefix(prefix) {
                    inner = l.dropFirst(prefix.count)
                } else if quoted {
                    break
                } else {
                    // a shorter indent inside a list: take what indentation there is
                    inner = l.drop { $0 == " " || $0 == "\t" }
                }
                lines.removeFirst()
                raw.append(l)
                let lt = inner.drop { $0 == " " || $0 == "\t" }
                if lt.hasPrefix(fence), lt.drop(while: { $0 == fenceChar }).allSatisfy(\.isWhitespace) {
                    closed = true
                    break
                }
                body.append(inner)
            }
            if let lang = info.normalizedLanguage, let kind = ShareVisual.Kind(language: lang) {
                pieces.append(.visual(ShareVisual(kind: kind, source: body.joined(separator: "\n"), theme: theme, tables: tables)))
                out.append(String(prefix) + token(startAt + pieces.count - 1))
            } else {
                out.append(String(line))
                out.append(contentsOf: raw.map(String.init))
            }
            _ = closed
        }
        return (out.joined(separator: "\n"), pieces)
    }

    /// `![alt](http(s)://… "title")` → a placeholder. Inline code spans are
    /// skipped; parentheses in the URL may nest (`(a_(b))`), and an
    /// `<…>` destination may hold anything but `>`.
    static func images(in line: String, pieces: inout [Piece], startAt: Int) -> String {
        guard line.contains("![") else { return line }
        var result = ""
        var rest = Substring(line)
        while let open = rest.range(of: "![") {
            result += rest[..<open.lowerBound]
            let ticks = result.filter { $0 == "`" }.count
            guard ticks % 2 == 0, let closeAlt = rest[open.upperBound...].firstIndex(of: "]"),
                  rest.index(after: closeAlt) < rest.endIndex, rest[rest.index(after: closeAlt)] == "(",
                  let (target, closeParen) = destination(rest, from: rest.index(closeAlt, offsetBy: 2))
            else {
                result += "!["
                rest = rest[open.upperBound...]
                continue
            }
            let alt = String(rest[open.upperBound..<closeAlt])
            if let url = URL(string: target), let scheme = url.scheme?.lowercased(), scheme == "https" || scheme == "http", url.host() != nil {
                pieces.append(.image(alt: alt, url: url, original: target.contains(" ") || target.contains("(") || target.contains(")") ? "<\(target)>" : target))
                result += token(startAt + pieces.count - 1)
            } else {
                result += rest[open.lowerBound...closeParen]
            }
            rest = rest[rest.index(after: closeParen)...]
        }
        return result + rest
    }

    /// The link destination starting at `start` (just after `(`) and the
    /// index of the `)` that closes it, skipping an optional title.
    static func destination(_ s: Substring, from start: Substring.Index) -> (String, Substring.Index)? {
        var i = start
        while i < s.endIndex, s[i] == " " { i = s.index(after: i) }
        var target = ""
        if i < s.endIndex, s[i] == "<" {
            guard let close = s[i...].firstIndex(of: ">") else { return nil }
            target = String(s[s.index(after: i)..<close])
            i = s.index(after: close)
        } else {
            var depth = 0
            while i < s.endIndex {
                let c = s[i]
                if c == "(" { depth += 1 }
                if c == ")" {
                    if depth == 0 { break }
                    depth -= 1
                }
                if c == " " && depth == 0 { break }
                target.append(c)
                i = s.index(after: i)
            }
        }
        // an optional "title" or 'title', then the closing paren
        while i < s.endIndex, s[i] == " " { i = s.index(after: i) }
        if i < s.endIndex, s[i] == "\"" || s[i] == "'" {
            let q = s[i]
            guard let endQ = s[s.index(after: i)...].firstIndex(of: q) else { return nil }
            i = s.index(after: endQ)
            while i < s.endIndex, s[i] == " " { i = s.index(after: i) }
        }
        guard i < s.endIndex, s[i] == ")" else { return nil }
        return (target, i)
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

struct ShareRenderFailure: Error, Sendable {
    var message: String
}
