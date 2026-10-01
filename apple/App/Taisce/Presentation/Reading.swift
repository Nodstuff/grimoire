import Foundation
import TaisceKit

/// A doc block ready to draw, with its markdown (for checkbox toggles).
struct DocBlock: Identifiable, Hashable, Sendable {
    var id: BlockID
    var content: String
    var nodes: [RenderNode]
}

/// A doc as the reading view shows it: frontmatter becomes tag chips, and a
/// leading H1 repeating the title is dropped (the view draws the title).
struct DocPage: Hashable, Sendable {
    var tags: [String] = []
    var blocks: [DocBlock] = []

    var isEmpty: Bool { blocks.isEmpty }

    /// Pure and synchronous (markdown parsing): call it off the main actor.
    static func build(title: String, blocks: [Block], render: (Block) -> [RenderNode] = BlockRenderer.render) -> DocPage {
        var page = DocPage()
        for b in blocks where !b.deleted && b.blockType != .comment {
            // frontmatter, whatever type the import gave it
            let trimmed = b.content.trimmingCharacters(in: .whitespacesAndNewlines)
            if page.blocks.isEmpty, trimmed.hasPrefix("---\n"), trimmed.hasSuffix("\n---") {
                page.tags.append(contentsOf: frontmatterTags(trimmed))
                continue
            }
            var nodes = render(b)
            nodes.removeAll { node in
                if case .frontmatter(let text) = node {
                    page.tags.append(contentsOf: frontmatterTags(text))
                    return true
                }
                return false
            }
            if page.blocks.isEmpty, case .heading(1, let inline)? = nodes.first,
               inline.trimmingCharacters(in: .whitespaces).caseInsensitiveCompare(title) == .orderedSame {
                nodes.removeFirst()
            }
            if !nodes.isEmpty { page.blocks.append(DocBlock(id: b.id, content: b.content, nodes: nodes)) }
        }
        var seen: Set<String> = []
        page.tags = page.tags.filter { seen.insert($0).inserted }
        return page
    }

    /// `tags:` list items in a `---` block, lowercased, as the store reads them
    /// (`frontmatter_tags` in crates/store/src/sqlite.rs); also `tags: [a, b]`.
    static func frontmatterTags(_ content: String) -> [String] {
        guard content.hasPrefix("---") else { return [] }
        var out: [String] = []
        var inTags = false
        func clean(_ s: some StringProtocol) -> String {
            s.trimmingCharacters(in: .whitespaces).trimmingCharacters(in: CharacterSet(charactersIn: "\"'#")).lowercased()
        }
        for line in content.split(separator: "\n", omittingEmptySubsequences: false) {
            if inTags {
                let t = line.drop { $0 == " " || $0 == "\t" }
                if t.hasPrefix("- ") {
                    out.append(clean(t.dropFirst(2)))
                    continue
                }
                inTags = false
            }
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed == "tags:" {
                inTags = true
            } else if trimmed.hasPrefix("tags:"), let open = trimmed.firstIndex(of: "["), let close = trimmed.lastIndex(of: "]"), open < close {
                out += trimmed[trimmed.index(after: open)..<close].split(separator: ",").map(clean)
            }
        }
        return out.filter { !$0.isEmpty }
    }
}

/// Task-list checkboxes inside one block's markdown, in document order.
enum Checkbox {
    /// The markdown with the `index`th checkbox set to `checked`; nil when
    /// the block has no such checkbox.
    static func toggled(_ content: String, index: Int, checked: Bool) -> String? {
        var lines = content.split(separator: "\n", omittingEmptySubsequences: false).map(String.init)
        var n = 0
        var inFence = false
        for (i, line) in lines.enumerated() {
            let t = line.drop { $0 == " " || $0 == "\t" || $0 == ">" }
            if t.hasPrefix("```") || t.hasPrefix("~~~") { inFence.toggle() }
            guard !inFence, let box = boxRange(in: line) else { continue }
            if n == index {
                lines[i].replaceSubrange(box, with: checked ? "[x]" : "[ ]")
                return lines.joined(separator: "\n")
            }
            n += 1
        }
        return nil
    }

    /// The `[ ]` / `[x]` after a list marker (`-`, `*`, `+`, `1.`, `1)`).
    static func boxRange(in line: String) -> Range<String.Index>? {
        var i = line.startIndex
        while i < line.endIndex, line[i] == " " || line[i] == "\t" || line[i] == ">" { i = line.index(after: i) }
        guard i < line.endIndex else { return nil }
        if "-*+".contains(line[i]) {
            i = line.index(after: i)
        } else {
            var digits = 0
            while i < line.endIndex, line[i].isASCII, line[i].isNumber { i = line.index(after: i); digits += 1 }
            guard digits > 0, i < line.endIndex, line[i] == "." || line[i] == ")" else { return nil }
            i = line.index(after: i)
        }
        guard i < line.endIndex, line[i] == " " else { return nil }
        while i < line.endIndex, line[i] == " " { i = line.index(after: i) }
        let rest = line[i...]
        guard rest.count >= 3, rest.hasPrefix("[") else { return nil }
        let mark = line.index(after: i)
        let close = line.index(after: mark)
        guard " xX".contains(line[mark]), line[close] == "]" else { return nil }
        return i..<line.index(after: close)
    }
}

/// One search result card (one per doc: its best-ranked block).
struct SearchResult: Identifiable, Hashable, Sendable {
    var docID: DocID
    var blockID: BlockID
    var title: String
    var breadcrumb: String?
    var snippet: String
    var tags: [String]
    var id: DocID { docID }
}

struct SearchState: Hashable, Sendable {
    var results: [SearchResult] = []
    var offline = false

    /// Tags across the results, most common first (then by name).
    var tags: [String] {
        var count: [String: Int] = [:]
        for r in results { for t in Set(r.tags) { count[t, default: 0] += 1 } }
        return count.sorted { ($1.value, $0.key) < ($0.value, $1.key) }.map(\.key)
    }

    func filtered(by tag: String?) -> [SearchResult] {
        guard let tag else { return results }
        return results.filter { $0.tags.contains(tag) }
    }

    /// Hits are ranked best first; keep the first per doc.
    static func build(
        hits: [(block: BlockID, doc: DocID, title: String, content: String)],
        query: String, index: DocIndex, tags: [DocID: [String]], offline: Bool = false
    ) -> SearchState {
        var seen: Set<DocID> = []
        let results = hits.compactMap { h -> SearchResult? in
            guard seen.insert(h.doc).inserted else { return nil }
            let title = h.title.isEmpty ? (index.byID[h.doc]?.title ?? "Untitled") : h.title
            return SearchResult(
                docID: h.doc, blockID: h.block, title: title, breadcrumb: index.breadcrumb(of: h.doc),
                snippet: snippet(h.content, query: query), tags: tags[h.doc] ?? []
            )
        }
        return SearchState(results: results, offline: offline)
    }

    /// Markdown → a plain line around the first match (about `radius`
    /// characters each side), with ellipses where it was cut.
    static func snippet(_ markdown: String, query: String, radius: Int = 70) -> String {
        let plain = plainText(markdown)
        guard let first = terms(query).compactMap({ plain.range(of: $0, options: [.caseInsensitive, .diacriticInsensitive]) })
            .min(by: { $0.lowerBound < $1.lowerBound })
        else { return String(plain.prefix(radius * 2)) + (plain.count > radius * 2 ? "…" : "") }
        let start = plain.index(first.lowerBound, offsetBy: -radius, limitedBy: plain.startIndex) ?? plain.startIndex
        let end = plain.index(first.upperBound, offsetBy: radius, limitedBy: plain.endIndex) ?? plain.endIndex
        // don't cut a word in half at the front
        var s = start
        if s > plain.startIndex, let space = plain[s..<first.lowerBound].firstIndex(of: " ") { s = plain.index(after: space) }
        return (s > plain.startIndex ? "…" : "") + plain[s..<end] + (end < plain.endIndex ? "…" : "")
    }

    static func terms(_ query: String) -> [String] {
        query.split(whereSeparator: \.isWhitespace).map { $0.trimmingCharacters(in: CharacterSet(charactersIn: "\"*")) }.filter { !$0.isEmpty }
    }

    /// Every case-insensitive occurrence of each query term.
    static func matches(in text: String, query: String) -> [Range<String.Index>] {
        var out: [Range<String.Index>] = []
        for term in terms(query) {
            var from = text.startIndex
            while from < text.endIndex, let r = text.range(of: term, options: [.caseInsensitive, .diacriticInsensitive], range: from..<text.endIndex) {
                out.append(r)
                from = r.upperBound
            }
        }
        return out.sorted { $0.lowerBound < $1.lowerBound }
    }

    /// Strips the markdown a snippet shouldn't show: heading/list/quote
    /// markers, emphasis, code ticks, link targets, wikilink brackets.
    static func plainText(_ md: String) -> String {
        var lines: [String] = []
        for raw in md.split(separator: "\n") {
            var l = raw.trimmingCharacters(in: .whitespaces)
            if l.hasPrefix("```") || l == "---" { continue }
            while let f = l.first, "#>".contains(f) { l = String(l.dropFirst()).trimmingCharacters(in: .whitespaces) }
            for marker in ["- [ ] ", "- [x] ", "- ", "* ", "+ "] where l.hasPrefix(marker) {
                l = String(l.dropFirst(marker.count))
                break
            }
            if l.hasPrefix("|") { l = l.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.allSatisfy { "-: ".contains($0) } }.joined(separator: " · ") }
            lines.append(l)
        }
        var s = lines.joined(separator: " ")
        s = s.replacingOccurrences(of: #"\[\[([^\]|]*\|)?([^\]]*)\]\]"#, with: "$2", options: .regularExpression)
        s = s.replacingOccurrences(of: #"\[([^\]]*)\]\([^)]*\)"#, with: "$1", options: .regularExpression)
        for token in ["**", "__", "`", "~~"] { s = s.replacingOccurrences(of: token, with: "") }
        s = s.replacingOccurrences(of: #"(?<![\w*])\*(?!\s)([^*]+)\*"#, with: "$1", options: .regularExpression)
        return s.replacingOccurrences(of: #"\s+"#, with: " ", options: .regularExpression).trimmingCharacters(in: .whitespaces)
    }
}
