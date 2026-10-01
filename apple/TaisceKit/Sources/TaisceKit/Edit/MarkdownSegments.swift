import Foundation

/// Split markdown into block sources the way the server's importer does
/// (`segment` in crates/store/src/import.rs): blank lines separate blocks,
/// except inside a fence; a heading is a block of its own.
public enum MarkdownSegments {
    public static func split(_ md: String) -> [String] {
        var out: [String] = []
        var cur: [Substring] = []
        var fence: Substring?
        for line in md.replacingOccurrences(of: "\r\n", with: "\n").split(separator: "\n", omittingEmptySubsequences: false) {
            let t = line.drop { $0 == " " }
            if let f = fence {
                cur.append(line)
                if t.hasPrefix(f) { fence = nil }
                continue
            }
            if t.hasPrefix("```") || t.hasPrefix("~~~") {
                if !cur.isEmpty, !cur.allSatisfy({ $0.isEmpty }) { out.append(cur.joined(separator: "\n")); cur = [] }
                fence = t.prefix(3)
                cur.append(line)
                continue
            }
            if t.isEmpty {
                if !cur.isEmpty { out.append(cur.joined(separator: "\n")); cur = [] }
                continue
            }
            // a heading is its own block
            if t.hasPrefix("#"), EditorBlockContent.headingInline(String(t)) != nil {
                if !cur.isEmpty { out.append(cur.joined(separator: "\n")); cur = [] }
                out.append(String(line))
                continue
            }
            cur.append(line)
        }
        if !cur.isEmpty { out.append(cur.joined(separator: "\n")) }
        return out.map { $0.trimmingCharacters(in: .newlines) }.filter { !$0.isEmpty }
    }
}
