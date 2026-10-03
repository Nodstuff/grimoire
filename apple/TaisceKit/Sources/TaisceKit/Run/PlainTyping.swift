import Foundation

/// Code typed into a SwiftUI text field (the try line, Edit to try) goes
/// through the system's smart quotes and dashes, which SwiftUI can't turn
/// off: `"abc"` arrives as `“abc”` and Go refuses it. `straighten` undoes
/// that in what changed between two values of the field (what was just
/// typed or pasted), leaving text that was already there alone, so a curly
/// quote inside an existing string literal survives.
public enum PlainTyping {
    static let replacements: [Character: String] = [
        "\u{201C}": "\"", "\u{201D}": "\"", "\u{201E}": "\"",
        "\u{2018}": "'", "\u{2019}": "'", "\u{201A}": "'",
        "\u{2014}": "--", "\u{2013}": "-",
    ]

    public static func straighten(old: String, new: String) -> String {
        let o = Array(old), n = Array(new)
        var prefix = 0
        while prefix < o.count, prefix < n.count, o[prefix] == n[prefix] { prefix += 1 }
        var suffix = 0
        while suffix < o.count - prefix, suffix < n.count - prefix, o[o.count - 1 - suffix] == n[n.count - 1 - suffix] { suffix += 1 }
        let inserted = n[prefix..<(n.count - suffix)]
        guard inserted.contains(where: { replacements[$0] != nil }) else { return new }
        let fixed = inserted.map { replacements[$0] ?? String($0) }.joined()
        return String(n[..<prefix]) + fixed + String(n[(n.count - suffix)...])
    }
}
