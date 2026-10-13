import UIKit
import UniformTypeIdentifiers

/// Rich text from another app (Safari, Mail, Notes) as markdown: links keep
/// their hrefs, bold and italic their marks, paragraphs their breaks. The
/// surface pastes the markdown (`Surface.paste`).
enum RichPaste {
    /// Only rich text that carries links is converted: code and plain
    /// prose copied from an editor, a terminal or GitHub also put HTML/RTF
    /// on the pasteboard, and converting them would trim indentation, split
    /// every line into a block and read `#`, `*`, `[` as markdown.
    static func markdown(from pb: UIPasteboard) -> String? {
        let html = UTType.html.identifier, rtf = UTType.rtf.identifier
        if let data = pb.data(forPasteboardType: html) {
            guard data.range(of: Data("href".utf8)) != nil else { return nil }
            return markdown(data: data, type: .html)
        }
        if let data = pb.data(forPasteboardType: rtf) {
            guard data.range(of: Data("HYPERLINK".utf8)) != nil else { return nil }
            return markdown(data: data, type: .rtf)
        }
        return nil
    }

    static func markdown(data: Data, type: NSAttributedString.DocumentType) -> String? {
        guard let a = try? NSAttributedString(data: data, options: [.documentType: type, .characterEncoding: String.Encoding.utf8.rawValue],
                                              documentAttributes: nil),
              worthConverting(a) else { return nil }
        return markdown(from: a)
    }

    /// Prose with at least one link and no monospaced (code) runs.
    static func worthConverting(_ a: NSAttributedString) -> Bool {
        var link = false, code = false
        a.enumerateAttributes(in: NSRange(location: 0, length: a.length)) { attrs, _, _ in
            if attrs[.link] != nil { link = true }
            if let font = attrs[.font] as? UIFont, font.fontDescriptor.symbolicTraits.contains(.traitMonoSpace) { code = true }
        }
        return link && !code
    }

    static func markdown(from a: NSAttributedString) -> String {
        var out = ""
        let ns = a.string as NSString
        a.enumerateAttributes(in: NSRange(location: 0, length: a.length)) { attrs, r, _ in
            var t = ns.substring(with: r)
            let trimmed = t.trimmingCharacters(in: .whitespacesAndNewlines)
            guard !trimmed.isEmpty, !t.contains("\n") else { out += t; return }
            let lead = String(t.prefix { $0 == " " })
            let tail = String(t.reversed().prefix { $0 == " " })
            t = trimmed
            if let font = attrs[.font] as? UIFont {
                let traits = font.fontDescriptor.symbolicTraits
                if traits.contains(.traitBold) { t = "**\(t)**" }
                if traits.contains(.traitItalic) { t = "*\(t)*" }
            }
            if let url = (attrs[.link] as? URL)?.absoluteString ?? (attrs[.link] as? String) { t = "[\(t)](\(url))" }
            out += lead + t + tail
        }
        // one line per paragraph in rich text: blank-line separated blocks in markdown
        let lines = out.split(separator: "\n", omittingEmptySubsequences: true).map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
        return lines.joined(separator: "\n\n")
    }
}
