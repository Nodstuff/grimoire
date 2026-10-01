import UIKit
import TaisceKit

/// The bar above the keyboard: bold, italic, inline code, `[[`, to-do,
/// block type (with indent / outdent), hide keyboard. It acts on whichever block
/// has the caret.
@MainActor
final class FormattingBar: UIToolbar {
    unowned let model: EditorModel
    private var bold: UIBarButtonItem!
    private var italic: UIBarButtonItem!
    private var code: UIBarButtonItem!
    private var wiki: UIBarButtonItem!
    private var todo: UIBarButtonItem!
    private var kind: UIBarButtonItem!

    init(model: EditorModel) {
        self.model = model
        super.init(frame: CGRect(x: 0, y: 0, width: 320, height: 44))
        sizeToFit()
        tintColor = UIColor(Theme.accentActive)
        accessibilityIdentifier = "editor.bar"

        func item(_ symbol: String, _ label: String, _ id: String, _ action: @escaping () -> Void) -> UIBarButtonItem {
            let b = UIBarButtonItem(image: UIImage(systemName: symbol), primaryAction: UIAction { _ in action() })
            b.accessibilityLabel = label
            b.accessibilityIdentifier = id
            return b
        }
        bold = item("bold", "Bold", "bar.bold") { [weak self] in self?.model.activeCoordinator?.toggle(.bold) }
        italic = item("italic", "Italic", "bar.italic") { [weak self] in self?.model.activeCoordinator?.toggle(.italic) }
        code = item("chevron.left.forwardslash.chevron.right", "Inline code", "bar.code") { [weak self] in self?.model.activeCoordinator?.toggle(.code) }
        wiki = UIBarButtonItem(title: "[[ ]]", primaryAction: UIAction { [weak self] _ in self?.model.activeCoordinator?.insertWikiOpener() })
        wiki.accessibilityLabel = "Link to a doc"
        wiki.accessibilityIdentifier = "bar.wiki"
        todo = item("checklist", "To-do", "bar.todo") { [weak self] in self?.model.toggleTodo() }
        kind = UIBarButtonItem(image: UIImage(systemName: "textformat.size"), menu: kindMenu())
        kind.accessibilityLabel = "Block type"
        kind.accessibilityIdentifier = "bar.kind"
        let hide = item("keyboard.chevron.compact.down", "Hide keyboard", "bar.hide") { [weak self] in
            self?.model.activeCoordinator?.textView?.resignFirstResponder()
        }
        // iOS 26 groups bar items into capsules and overflows the rest:
        // indent lives in the block menu so Hide keyboard stays in view
        items = [bold, italic, code, wiki, todo, kind, .flexibleSpace(), hide]
    }

    required init?(coder: NSCoder) { fatalError("not from a nib") }

    private func kindMenu() -> UIMenu {
        let choices: [(BlockKindChoice, String, String)] = [
            (.paragraph, "Text", "text.alignleft"),
            (.heading1, "Heading 1", "textformat.size.larger"),
            (.heading2, "Heading 2", "textformat.size"),
            (.heading3, "Heading 3", "textformat.size.smaller"),
            (.bullet, "Bulleted list", "list.bullet"),
            (.numbered, "Numbered list", "list.number"),
            (.todo, "To-do list", "checklist"),
            (.quote, "Quote", "text.quote"),
        ]
        let kinds = choices.map { choice, title, symbol in
            UIAction(title: title, image: UIImage(systemName: symbol)) { [weak self] _ in self?.model.setKind(choice) }
        }
        let nesting = UIMenu(options: .displayInline, children: [
            UIAction(title: "Indent", image: UIImage(systemName: "increase.indent")) { [weak self] _ in self?.model.activeCoordinator?.indent(+1) },
            UIAction(title: "Outdent", image: UIImage(systemName: "decrease.indent")) { [weak self] _ in self?.model.activeCoordinator?.indent(-1) },
        ])
        return UIMenu(title: "Block type", children: kinds + [nesting])
    }

    /// Enable what applies to the focused block; show the marks in effect.
    func update(for content: EditorBlockContent, marks: InlineMarks) {
        let text = !content.isRaw
        for b in [bold, italic, code, wiki, todo] { b?.isEnabled = text }
        kind.isEnabled = text || content.isRaw
        bold.isSelected = marks.contains(.bold)
        italic.isSelected = marks.contains(.italic)
        code.isSelected = marks.contains(.code)
        bold.accessibilityValue = bold.isSelected ? "On" : "Off"
        italic.accessibilityValue = italic.isSelected ? "On" : "Off"
        code.accessibilityValue = code.isSelected ? "On" : "Off"
    }
}
