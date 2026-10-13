import UIKit
import UniformTypeIdentifiers

/// Rich text from another app (Safari, Mail, Notes) as markdown: links keep
/// their hrefs, bold and italic their marks, paragraphs their breaks. The
/// surface pastes the markdown (`Surface.paste`).
enum RichPaste {
    static func markdown(from pb: UIPasteboard) -> String? {
        let html = UTType.html.identifier, rtf = UTType.rtf.identifier
        if let data = pb.data(forPasteboardType: html) { return markdown(data: data, type: .html) }
        if let data = pb.data(forPasteboardType: rtf) { return markdown(data: data, type: .rtf) }
        return nil
    }

    static func markdown(data: Data, type: NSAttributedString.DocumentType) -> String? {
        guard let a = try? NSAttributedString(data: data, options: [.documentType: type, .characterEncoding: String.Encoding.utf8.rawValue],
                                              documentAttributes: nil) else { return nil }
        return markdown(from: a)
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
