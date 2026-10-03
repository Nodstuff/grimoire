import Foundation

/// A practice edit saved back to the doc: the block's markdown with the
/// fenced code replaced, the fence's info string kept. Pure.
public enum FenceEdit {
    /// nil when `content` has no fenced code block.
    public static func replacingCode(in content: String, with code: String) -> String? {
        let lines = content.components(separatedBy: "\n")
        guard let open = lines.firstIndex(where: { fence($0) != nil }), let (ch, len, info) = fence(lines[open]) else { return nil }
        let close = lines.indices.last { i in
            guard i > open, let f = fence(lines[i]) else { return false }
            return f.char == ch && f.length >= len && f.info.trimmingCharacters(in: .whitespaces).isEmpty
        }
        let body = code.components(separatedBy: "\n")
        // a body line that would close the fence: lengthen the fence
        let longest = body.compactMap { fence($0) }.filter { $0.char == ch }.map(\.length).max() ?? 0
        let n = max(len, longest + 1)
        let marker = String(repeating: ch, count: n)
        let indent = String(lines[open].prefix { $0 == " " })
        var out = Array(lines[..<open])
        out.append(indent + marker + info)
        out += body
        out.append(indent + marker)
        if let close { out += lines[(close + 1)...] }
        return out.joined(separator: "\n")
    }

    /// The block with `key=value` set on its first fence's info string
    /// (replacing that key if present, else appended), e.g. `db=analytics`
    /// on ```` ```sql ````. The code is untouched. nil when `content` has no
    /// fenced code block, or its fence names no language.
    public static func settingAttribute(_ key: String, _ value: String, in content: String) -> String? {
        var lines = content.components(separatedBy: "\n")
        guard let open = lines.firstIndex(where: { fence($0) != nil }), let (ch, len, info) = fence(lines[open]) else { return nil }
        let quoted = value.contains(where: \.isWhitespace) || value.isEmpty ? "\"\(value)\"" : value
        let words = FenceInfo.words(info)
        // a bare fence: the attribute would read as its language
        guard !words.isEmpty else { return nil }
        var parts: [String] = []
        // rebuild from the words, re-quoting any with spaces
        func q(_ w: String) -> String {
            guard let eq = w.firstIndex(of: "="), w.contains(where: \.isWhitespace) else { return w }
            return w[...eq] + "\"" + w[w.index(after: eq)...] + "\""
        }
        var replaced = false
        for (i, w) in words.enumerated() {
            if i > 0, let eq = w.firstIndex(of: "="), w[..<eq].lowercased() == key.lowercased() {
                if !replaced { parts.append("\(key)=\(quoted)") }
                replaced = true
            } else {
                parts.append(q(w))
            }
        }
        if !replaced { parts.append("\(key)=\(quoted)") }
        let indent = String(lines[open].prefix { $0 == " " })
        lines[open] = indent + String(repeating: ch, count: len) + parts.joined(separator: " ")
        return lines.joined(separator: "\n")
    }

    /// A fence line: up to three spaces, then three or more ` or ~.
    static func fence(_ line: String) -> (char: Character, length: Int, info: String)? {
        let indent = line.prefix { $0 == " " }
        guard indent.count <= 3 else { return nil }
        let rest = line.dropFirst(indent.count)
        guard let c = rest.first, c == "`" || c == "~" else { return nil }
        let run = rest.prefix { $0 == c }
        guard run.count >= 3 else { return nil }
        return (c, run.count, String(rest.dropFirst(run.count)))
    }
}
