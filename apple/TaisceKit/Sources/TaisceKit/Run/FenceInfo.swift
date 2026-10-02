import Foundation

/// A code fence's info string, split: the language (its first word) and
/// `key=value` attributes after it, e.g. ```` ```bash cwd=~/code/portus ````
/// or `cwd="~/My Code"`. cmark hands the whole info string over as the
/// block's "language"; this is the one place that reads it.
public struct FenceInfo: Sendable, Hashable {
    /// the first word, as written (nil for a bare fence)
    public var language: String?
    public var attributes: [String: String]

    public init(language: String?, attributes: [String: String] = [:]) {
        self.language = language
        self.attributes = attributes
    }

    public init(_ info: String?) {
        let words = Self.words(info ?? "")
        language = words.first
        var attrs: [String: String] = [:]
        for w in words.dropFirst() {
            guard let eq = w.firstIndex(of: "="), eq != w.startIndex else { continue }
            let key = w[..<eq].lowercased()
            attrs[key] = String(w[w.index(after: eq)...])
        }
        attributes = attrs
    }

    /// The language for matching (`bash`, `go`, …): lowercased.
    public var normalizedLanguage: String? { language?.lowercased() }

    /// `cwd=` with `~` expanded against `home`; nil when absent or empty.
    public func cwd(home: String) -> String? {
        guard let raw = attributes["cwd"], !raw.isEmpty else { return nil }
        return Self.expandTilde(raw, home: home)
    }

    public static func expandTilde(_ path: String, home: String) -> String {
        if path == "~" { return home }
        if path.hasPrefix("~/") { return home + path.dropFirst(1) }
        return path
    }

    /// Whitespace-separated words; a double- or single-quoted run is one
    /// word with its quotes removed (`cwd="a b"` → `cwd=a b`).
    static func words(_ s: String) -> [String] {
        var out: [String] = []
        var cur = ""
        var quote: Character?
        var inWord = false
        for ch in s {
            if let q = quote {
                if ch == q { quote = nil } else { cur.append(ch) }
                continue
            }
            if ch == "\"" || ch == "'" {
                quote = ch
                inWord = true
            } else if ch.isWhitespace {
                if inWord { out.append(cur) }
                cur = ""
                inWord = false
            } else {
                cur.append(ch)
                inWord = true
            }
        }
        if inWord { out.append(cur) }
        return out
    }
}
