import UIKit
import TaisceKit

/// The bar above the keyboard (the Lapis doc artboard): a glass capsule
/// with text style (Aa: kinds, bold, italic, code, indent), checklist,
/// bulleted list, `[[ ]]`, a due date, a code block, insert (+), and Hide
/// keyboard. It acts on the block under the caret. On the Mac it sits
/// above the doc (no software keyboard), without Hide keyboard.
@MainActor
final class FormattingBar: UIView {
    weak var model: SurfaceModel?
    private let glass: UIVisualEffectView
    private let stack = UIStackView()
    private var style: UIButton!
    private var todo: UIButton!
    private var bullets: UIButton!
    private var wiki: UIButton!
    private var due: UIButton!
    private var code: UIButton!
    private var insert: UIButton!
    /// what the Aa menu ticks (refreshed as the caret moves)
    private var marks = InlineMarks()
    private var content: EditorBlockContent = .paragraph(AttributedString())

    static let height: CGFloat = 60

    init(model: SurfaceModel) {
        self.model = model
        if #available(iOS 26.0, *) {
            glass = UIVisualEffectView(effect: UIGlassEffect())
        } else {
            glass = UIVisualEffectView(effect: UIBlurEffect(style: .systemChromeMaterial))
        }
        super.init(frame: CGRect(x: 0, y: 0, width: 320, height: Self.height))
        autoresizingMask = .flexibleHeight
        backgroundColor = .clear
        accessibilityIdentifier = "editor.bar"
        tintColor = UIColor.lapis.ink

        glass.translatesAutoresizingMaskIntoConstraints = false
        glass.layer.cornerRadius = 25
        glass.layer.cornerCurve = .continuous
        glass.clipsToBounds = true
        addSubview(glass)

        stack.axis = .horizontal
        stack.alignment = .center
        stack.spacing = 0
        stack.translatesAutoresizingMaskIntoConstraints = false
        glass.contentView.addSubview(stack)

        style = button(title: "Aa", label: "Text style", id: "bar.kind")
        style.showsMenuAsPrimaryAction = true
        style.menu = UIMenu(children: [UIDeferredMenuElement.uncached { [weak self] done in done(self?.styleMenu() ?? []) }])
        todo = button(symbol: "checklist", label: "Checklist", id: "bar.todo") { [weak self] in self?.model?.toggleTodo() }
        bullets = button(symbol: "list.bullet", label: "Bulleted list", id: "bar.bullets") { [weak self] in self?.toggleBullets() }
        wiki = button(title: "[[ ]]", label: "Link a doc", id: "bar.wiki", mono: true) { [weak self] in self?.model?.insertWikiOpener() }
        due = button(symbol: "calendar", label: "Due date", id: "bar.due")
        due.showsMenuAsPrimaryAction = true
        due.menu = dueMenu()
        code = button(symbol: "chevron.left.forwardslash.chevron.right", label: "Code block", id: "bar.codeBlock") { [weak self] in
            self?.model?.insertCodeBlock()
        }
        insert = button(symbol: "plus.circle", label: "Insert block", id: "bar.insert")
        insert.showsMenuAsPrimaryAction = true
        insert.menu = insertMenu()
        for b in [style, todo, bullets, wiki, due, code, insert] { stack.addArrangedSubview(b!) }
        let spacer = UIView()
        spacer.setContentHuggingPriority(.defaultLow, for: .horizontal)
        stack.addArrangedSubview(spacer)
        if !EditorChrome.barIsInline {
            let rule = UIView()
            rule.backgroundColor = UIColor.lapis.line2
            rule.translatesAutoresizingMaskIntoConstraints = false
            rule.widthAnchor.constraint(equalToConstant: 1).isActive = true
            rule.heightAnchor.constraint(equalToConstant: 24).isActive = true
            stack.addArrangedSubview(rule)
            stack.setCustomSpacing(4, after: rule)
            let hide = button(symbol: "keyboard.chevron.compact.down", label: "Hide keyboard", id: "bar.hide") { [weak self] in
                self?.model?.textView.resignFirstResponder()
            }
            stack.addArrangedSubview(hide)
        }

        let side: CGFloat = EditorChrome.barIsInline ? 0 : 10
        NSLayoutConstraint.activate([
            glass.leadingAnchor.constraint(equalTo: leadingAnchor, constant: side),
            glass.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -side),
            glass.topAnchor.constraint(equalTo: topAnchor, constant: 4),
            glass.heightAnchor.constraint(equalToConstant: 50),
            stack.leadingAnchor.constraint(equalTo: glass.contentView.leadingAnchor, constant: 4),
            stack.trailingAnchor.constraint(equalTo: glass.contentView.trailingAnchor, constant: -4),
            stack.topAnchor.constraint(equalTo: glass.contentView.topAnchor),
            stack.bottomAnchor.constraint(equalTo: glass.contentView.bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("not from a nib") }

    override var intrinsicContentSize: CGSize { CGSize(width: UIView.noIntrinsicMetric, height: Self.height) }

    private func button(symbol: String? = nil, title: String? = nil, label: String, id: String, mono: Bool = false,
                        action: (() -> Void)? = nil) -> UIButton {
        var c = UIButton.Configuration.plain()
        c.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 6, bottom: 0, trailing: 6)
        c.baseForegroundColor = mono ? UIColor.lapis.accent : UIColor.lapis.ink
        if let symbol {
            c.image = UIImage(systemName: symbol, withConfiguration: UIImage.SymbolConfiguration(pointSize: 17, weight: .regular))
        }
        if let title {
            var a = AttributeContainer()
            a.font = mono ? UIFont.monospacedSystemFont(ofSize: 15, weight: .semibold)
                : Newsreader.uiFont(size: 19, weight: .semibold)
            c.attributedTitle = AttributedString(title, attributes: a)
        }
        if mono {
            c.background.backgroundColor = UIColor.lapis.accentSoft
            c.background.cornerRadius = 20
            c.contentInsets = NSDirectionalEdgeInsets(top: 0, leading: 10, bottom: 0, trailing: 10)
        }
        let b = UIButton(configuration: c, primaryAction: action.map { a in UIAction { _ in a() } })
        b.accessibilityLabel = label
        b.accessibilityIdentifier = id
        b.translatesAutoresizingMaskIntoConstraints = false
        b.heightAnchor.constraint(equalToConstant: mono ? 40 : 44).isActive = true
        b.widthAnchor.constraint(greaterThanOrEqualToConstant: 42).isActive = true
        b.pointerStyleProvider = { _, effect, _ in UIPointerStyle(effect: effect) }
        return b
    }

    private func styleMenu() -> [UIMenuElement] {
        let isText = !content.isRaw
        func mark(_ title: String, _ symbol: String, _ m: InlineMarks) -> UIAction {
            let a = UIAction(title: title, image: UIImage(systemName: symbol), attributes: isText ? [] : .disabled,
                             state: marks.contains(m) ? .on : .off) { [weak self] _ in self?.model?.toggle(m) }
            return a
        }
        let inline = UIMenu(options: .displayInline, preferredElementSize: .small, children: [
            mark("Bold", "bold", .bold), mark("Italic", "italic", .italic), mark("Code", "chevron.left.forwardslash.chevron.right", .code),
        ])
        let choices: [(BlockKindChoice, String, String)] = [
            (.paragraph, "Text", "text.alignleft"),
            (.heading1, "Heading 1", "textformat.size.larger"),
            (.heading2, "Heading 2", "textformat.size"),
            (.heading3, "Heading 3", "textformat.size.smaller"),
            (.numbered, "Numbered list", "list.number"),
            (.quote, "Quote", "text.quote"),
        ]
        let kinds = UIMenu(options: .displayInline, children: choices.map { choice, title, symbol in
            UIAction(title: title, image: UIImage(systemName: symbol), state: Self.kind(of: content) == choice ? .on : .off) { [weak self] _ in
                self?.model?.setKind(choice)
            }
        })
        let nesting = UIMenu(options: .displayInline, children: [
            UIAction(title: "Indent", image: UIImage(systemName: "increase.indent")) { [weak self] _ in self?.model?.indent(+1) },
            UIAction(title: "Outdent", image: UIImage(systemName: "decrease.indent")) { [weak self] _ in self?.model?.indent(-1) },
        ])
        return [inline, kinds, nesting]
    }

    /// The kind a block is, for the Aa menu's tick.
    static func kind(of c: EditorBlockContent) -> BlockKindChoice? {
        switch c {
        case .paragraph: return .paragraph
        case let .heading(level, _): return level <= 1 ? .heading1 : level == 2 ? .heading2 : .heading3
        case .quote: return .quote
        case let .list(items):
            guard let first = items.first else { return nil }
            if first.prefix.checkbox != nil { return .todo }
            return first.prefix.ordered ? .numbered : .bullet
        case .raw: return nil
        }
    }

    private func toggleBullets() {
        model?.setKind(Self.kind(of: content) == .bullet ? .paragraph : .bullet)
    }

    private func dueMenu() -> UIMenu {
        let cal = Calendar.current
        let today = cal.startOfDay(for: .now)
        func day(_ title: String, _ date: Date?, _ symbol: String) -> UIAction {
            UIAction(title: title, image: UIImage(systemName: symbol)) { [weak self] _ in
                if let date { self?.model?.appendDue(date) }
            }
        }
        let nextMonday = cal.nextDate(after: today, matching: DateComponents(weekday: 2), matchingPolicy: .nextTime)
        return UIMenu(title: "Due", children: [
            day("Today", today, "sun.max"),
            day("Tomorrow", cal.date(byAdding: .day, value: 1, to: today), "sunrise"),
            day("Next Monday", nextMonday, "calendar"),
            day("In a week", cal.date(byAdding: .day, value: 7, to: today), "calendar.badge.clock"),
        ])
    }

    private func insertMenu() -> UIMenu {
        func block(_ title: String, _ symbol: String, _ markdown: String, source: Bool) -> UIAction {
            UIAction(title: title, image: UIImage(systemName: symbol)) { [weak self] _ in self?.model?.insertBlock(markdown, openSource: source) }
        }
        return UIMenu(title: "Insert", children: [
            UIAction(title: "To-do", image: UIImage(systemName: "checklist")) { [weak self] _ in self?.model?.setKind(.todo) },
            UIAction(title: "Numbered list", image: UIImage(systemName: "list.number")) { [weak self] _ in self?.model?.setKind(.numbered) },
            UIAction(title: "Quote", image: UIImage(systemName: "text.quote")) { [weak self] _ in self?.model?.setKind(.quote) },
            block("Table", "tablecells", "| Column | Column |\n| --- | --- |\n|  |  |", source: true),
            block("Diagram", "point.3.connected.trianglepath.dotted", "```mermaid\ngraph LR\n  A --> B\n```", source: true),
            block("Divider", "minus", "---", source: false),
        ])
    }

    /// Enable what applies to the focused block; show the marks in effect.
    func update(for content: EditorBlockContent, marks: InlineMarks) {
        self.content = content
        self.marks = marks
        let text = !content.isRaw
        for b in [todo, bullets, wiki, due] { b?.isEnabled = text }
        style?.isEnabled = true
        let isBullets = Self.kind(of: content) == .bullet
        bullets?.isSelected = isBullets
        bullets?.accessibilityValue = isBullets ? "On" : "Off"
        style?.accessibilityValue = [marks.contains(.bold) ? "bold" : nil, marks.contains(.italic) ? "italic" : nil, marks.contains(.code) ? "code" : nil]
            .compactMap { $0 }.joined(separator: ", ")
    }
}
