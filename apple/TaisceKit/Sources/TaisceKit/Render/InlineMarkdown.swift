import Foundation

/// Inline markdown → `AttributedString` for SwiftUI `Text`. `[[Wiki Links]]`
/// become `taisce://wiki/<title>` links the app resolves against the tree.
public enum InlineMarkdown {
    public static let wikiScheme = "taisce"

    public static func attributed(_ inline: String) -> AttributedString {
        let md = rewriteWikiLinks(inline)
        let opts = AttributedString.MarkdownParsingOptions(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        return (try? AttributedString(markdown: md, options: opts)) ?? AttributedString(inline)
    }

    /// `[[A/B|label]]` → `[label](taisce://wiki/A%2FB)`; `[[A]]` → `[A](...)`.
    public static func rewriteWikiLinks(_ s: String) -> String {
        var out = ""
        var rest = Substring(s)
        while let open = rest.range(of: "[["), let close = rest[open.upperBound...].range(of: "]]") {
            out += rest[..<open.lowerBound]
            let inner = rest[open.upperBound..<close.lowerBound]
            let parts = inner.split(separator: "|", maxSplits: 1)
            let target = String(parts.first ?? inner)
            let label = parts.count > 1 ? String(parts[1]) : target
            let encoded = target.addingPercentEncoding(withAllowedCharacters: .alphanumerics) ?? target
            out += "[\(label)](\(wikiScheme)://wiki/\(encoded))"
            rest = rest[close.upperBound...]
        }
        out += rest
        return out
    }

    /// The wiki title a `taisce://wiki/...` URL points at.
    public static func wikiTarget(_ url: URL) -> String? {
        guard url.scheme == wikiScheme, url.host() == "wiki" else { return nil }
        return String(url.path(percentEncoded: false).dropFirst())
    }
}
