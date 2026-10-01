import Foundation
import Testing
@testable import TaisceKit

/// Markdown ↔ the editor's model. The contract: a block the editor shows
/// structured serialises back to the same markdown meaning (so opening a
/// doc to edit never changes it), and the model survives its own
/// serialisation exactly (so a save never drifts).
@Suite(.timeLimit(.minutes(1))) struct EditorCodecTests {
    /// Real-world block sources; `true` = edited as rich text, `false` =
    /// kept as raw source, nil = either is fine (the invariants still hold).
    static let corpus: [(String, Bool?)] = [
        ("Plain paragraph.", true),
        ("**bold** and *italic* and `code`", true),
        ("***bold italic***", true),
        ("**bold with *nested italic* inside**", true),
        ("*italic with **nested bold** inside*", true),
        ("_underscore italic_ and __underscore bold__", true),
        ("~~struck~~ text", true),
        ("A [link](https://example.com) here", true),
        ("[Wiki](https://en.wikipedia.org/wiki/Foo_(bar))", true),
        ("[spaced](<https://example.com/a b>)", true),
        ("<https://example.com/auto>", true),
        ("See [[Roadmap]] for details", true),
        ("[[Grimoire/Architecture|the architecture]]", true),
        ("[[Doc#^abc123]]", true),
        ("[[Doc#Heading]] and [[Other]]", true),
        ("**[[Bold Wiki]]** and *[[Doc|italic alias]]*", true),
        ("[[Doc*with*stars]] stays literal inside", true),
        ("\\*not italic\\* and \\_not\\_ either", true),
        ("snake_case_identifier stays", true),
        ("Price: 5 * 3 = 15", true),
        ("emoji 🎉🚀 and flags 🇮🇪 and 👩🏽‍💻", true),
        ("中文段落，日本語のテキスト、한국어", true),
        ("مرحبا بالعالم — עברית with **bold**", true),
        ("Line one\nLine two (soft break)", true),
        ("**bold across\na soft break**", true),
        ("# Heading one", true),
        ("## Heading with **bold** and `code`", true),
        ("### Closing hashes ###", true),
        ("- item one\n- item two\n- item three", true),
        ("* star bullets\n* second", true),
        ("+ plus bullets\n+ second", true),
        ("1. first\n2. second\n3. third", true),
        ("3) paren numbered\n4) next", true),
        ("- [ ] open task\n- [x] done task", true),
        ("- parent\n  - child\n    - grandchild\n- back", true),
        ("1. step\n   - sub bullet\n2. step two", true),
        ("- [ ] task with [[Link]] and **bold** · due 2026-10-03", true),
        ("> A quote\n> continues here", true),
        ("> quote with *emphasis*", true),
        ("> [!NOTE]\n> A callout", false),
        ("```swift\nlet x = 1\n```", false),
        ("```mermaid\ngraph TD\n  A-->B\n```", false),
        ("| a | b |\n|---|---|\n| 1 | 2 |", false),
        ("![image](pic.png)", false),
        ("<div>html</div>", false),
        ("Hard break  \nnext", false),
        ("Backslash break\\\nnext", false),
        ("[titled](https://x.com \"Title\")", false),
        ("---", false),
        ("Text with <b>inline html</b>", false),
        ("Setext\n======", false),
        ("x <y> z", false),
        ("&copy; entity and &amp; ampersand", true),
        ("`code with `` backticks`", true),
        ("Footnote reference[^1]", true),
        ("Tab\tseparated", true),
        ("DECISION: we ship it", true),
        ("Multiple  spaces  inside", true),
        ("1986\\. A great year", true),
        ("\\# not a heading", true),
        ("**unclosed bold", true),
        ("Email <me@example.com>", nil),
        ("a_b_ c and 2*3*4", true),
        ("Quotes \"straight\" and 'single' -- dashes ---", true),
        ("Brackets [not a link] and [x]", true),
        ("    indented code", false),
        ("", true),
    ]

    @Test func corpusRoundTripsWithoutChangingMeaning() throws {
        #expect(Self.corpus.count >= 40)
        for (md, expectStructured) in Self.corpus {
            let content = EditorBlockContent.parse(markdown: md)
            if let expectStructured {
                #expect(!content.isRaw == expectStructured, "structured? \(md.debugDescription) → \(content)")
            }
            if case .raw(let s) = content {
                #expect(s == md, "raw keeps the bytes")
                continue
            }
            let out = content.markdown
            #expect(EditorBlockContent.sameMeaning(md, out), "meaning kept: \(md.debugDescription) → \(out.debugDescription)")
            // the model survives its own serialisation exactly
            #expect(EditorBlockContent.parse(markdown: out) == content, "stable: \(out.debugDescription)")
            #expect(EditorBlockContent.parse(markdown: out).markdown == out, "idempotent: \(out.debugDescription)")
        }
    }

    @Test func untouchedBlocksSendNothing() throws {
        let blocks = Self.corpus.enumerated().map { i, c in
            Block(id: "b\(i)", docID: "d", parentID: nil, orderKey: OrderKey.between(nil, nil) + String(repeating: "i", count: i), blockType: c.0.hasPrefix("#") ? .heading : c.0.hasPrefix("```") ? .code : .paragraph, content: c.0)
        }
        var session = EditorSession(editor: DocEditor(docID: "d", baseEpoch: 3, blocks: blocks))
        #expect(session.commitText().isEmpty, "opening a doc to edit writes nothing")
        // editing one block writes only that block
        let target = try #require(session.items.first { $0.content.markdown == "Plain paragraph." })
        session.update(target.id, content: .paragraph(AttributedString("Plain paragraph, edited.")))
        let reqs = session.commitText()
        #expect(reqs.count == 1)
        #expect(reqs.first?.ops.map(\.kind) == [.replace(target: target.id, content: "Plain paragraph, edited.")])
        #expect(session.commitText().isEmpty)
    }

    // MARK: the model

    func runs(_ md: String) throws -> [InlineRun] {
        InlineCodec.runs(try #require(InlineCodec.parse(md)))
    }

    @Test func nestedMarks() throws {
        #expect(try runs("**a *b* c**") == [
            InlineRun("a ", marks: .bold), InlineRun("b", marks: [.bold, .italic]), InlineRun(" c", marks: .bold),
        ])
        #expect(try runs("~~x `y`~~") == [InlineRun("x ", marks: .strike), InlineRun("y", marks: [.strike, .code])])
    }

    @Test func wikilinksKeepTheirSource() throws {
        #expect(try runs("see [[Doc|alias]] and [[Doc#^ab12]]") == [
            InlineRun("see "), InlineRun("alias", wiki: "Doc|alias"), InlineRun(" and "), InlineRun("Doc#^ab12", wiki: "Doc#^ab12"),
        ])
        // two identical wikilinks side by side stay two
        let twice = try #require(InlineCodec.parse("[[A]][[A]]"))
        #expect(InlineCodec.serialize(twice) == "[[A]][[A]]")
        // emphasis inside a title is part of the title
        #expect(try runs("[[A *b* c]]") == [InlineRun("A *b* c", wiki: "A *b* c")])
        // inside code it's just text
        #expect(try runs("`[[x]]`") == [InlineRun("[[x]]", marks: .code)])
        #expect(InlineCodec.serialize(try #require(InlineCodec.parse("`[[x]]`"))) == "`[[x]]`")
        // escaped brackets are not a wikilink
        #expect(try runs("\\[\\[x]]") == [InlineRun("[[x]]")])
    }

    @Test func linksWithParensAndSpaces() throws {
        #expect(try runs("[w](https://en.wikipedia.org/wiki/Foo_(bar))") == [InlineRun("w", link: "https://en.wikipedia.org/wiki/Foo_(bar)")])
        let spaced = InlineCodec.build([InlineRun("x", link: "a b")])
        #expect(InlineCodec.serialize(spaced) == "[x](<a b>)")
        let unbalanced = InlineCodec.build([InlineRun("x", link: "a)b")])
        #expect(InlineCodec.serialize(unbalanced) == "[x](<a)b>)")
        let auto = InlineCodec.build([InlineRun("https://x.org", link: "https://x.org")])
        #expect(InlineCodec.serialize(auto) == "<https://x.org>")
        let bold = InlineCodec.build([InlineRun("a ", link: "u"), InlineRun("b", marks: .bold, link: "u")])
        #expect(InlineCodec.serialize(bold) == "[a **b**](u)")
    }

    @Test func canonicalSerialisation() {
        func md(_ runs: InlineRun...) -> String { InlineCodec.serialize(InlineCodec.build(runs)) }
        #expect(md(InlineRun("bold", marks: .bold), InlineRun(" then "), InlineRun("it", marks: .italic)) == "**bold** then *it*")
        #expect(md(InlineRun("both", marks: [.bold, .italic])) == "***both***")
        #expect(md(InlineRun("a", marks: [.bold, .italic]), InlineRun(" b", marks: .italic)) == "***a** b*")
        // whitespace at a mark's edge goes outside the delimiters
        #expect(md(InlineRun("x"), InlineRun(" bold ", marks: .bold), InlineRun("y")) == "x **bold** y")
        #expect(md(InlineRun("tick ` inside", marks: .code)) == "``tick ` inside``")
        #expect(md(InlineRun("`edge", marks: .code)) == "`` `edge ``")
        #expect(md(InlineRun("a*b_c snake_case")) == "a\\*b_c snake_case", "intraword _ is literal")
        #expect(md(InlineRun("_lead and trail_")) == "\\_lead and trail\\_")
        #expect(md(InlineRun("# hash")) == "\\# hash")
        #expect(md(InlineRun("- dash")) == "\\- dash")
        #expect(md(InlineRun("1. one")) == "1\\. one")
        #expect(md(InlineRun("> quote")) == "\\> quote")
        #expect(md(InlineRun("[a](b)")) == "[a\\](b)")
        #expect(md(InlineRun("[[no link]]")) == "\\[\\[no link\\]\\]")
        #expect(md(InlineRun("x <tag> y")) == "x \\<tag> y")
        #expect(md(InlineRun("AT&T and &amp;")) == "AT&T and \\&amp;")
        #expect(md(InlineRun("~a~")) == "\\~a\\~")
        #expect(md(InlineRun("~5 min")) == "~5 min")
        #expect(md(InlineRun("C:\\path")) == "C:\\path")
        #expect(md(InlineRun("trailing \\")) == "trailing \\\\")
        #expect(md(InlineRun("  spaced  ")) == "spaced")
        #expect(md(InlineRun("a  \nb")) == "a\nb")
        #expect(md(InlineRun("Doc", wiki: "Doc")) == "[[Doc]]")
        #expect(md(InlineRun("alias", marks: .bold, wiki: "Doc|alias")) == "**[[Doc|alias]]**")
    }

    @Test func everyCanonicalOutputParsesBack() throws {
        // text with every awkward character, in every mark
        let nasty = "a*b_c`d[e]f<g>h&i;~j\\k#l|m!n"
        for marks in [InlineMarks(), .bold, .italic, .strike, .code, [.bold, .italic]] {
            let t = InlineCodec.build([InlineRun("x "), InlineRun(nasty, marks: marks), InlineRun(" y")])
            let out = InlineCodec.serialize(t)
            let back = try #require(InlineCodec.parse(out), "parses: \(out)")
            #expect(String(back.characters) == "x \(nasty) y", "text survives: \(out)")
        }
    }

    @Test func blockKinds() throws {
        #expect(EditorBlockContent.parse(markdown: "## Two") == .heading(level: 2, AttributedString("Two")))
        guard case let .list(items) = EditorBlockContent.parse(markdown: "- a\n  - b\n- [x] c") else {
            Issue.record("a list")
            return
        }
        #expect(items.map(\.prefix.indent) == [0, 1, 0])
        #expect(items.map(\.prefix.checkbox) == [nil, nil, true])
        #expect(items.map { String($0.text.characters) } == ["a", "b", "c"])
        guard case let .list(numbered) = EditorBlockContent.parse(markdown: "7. a\n8. b") else {
            Issue.record("a numbered list")
            return
        }
        #expect(numbered.map(\.prefix.number) == [7, 8])
        #expect(EditorBlockContent.parse(markdown: "> q\n> r") == .quote(AttributedString("q\nr")))
        // raw kinds by block type, whatever the text
        #expect(EditorBlockContent.parse(Block(id: "c", docID: "d", parentID: nil, orderKey: "i", blockType: .code, content: "plain")).isRaw)
    }

    @Test func listSerialisationRenumbersAndNests() {
        let items = [
            EditorListItem(ListPrefix(ordered: true, number: 1), AttributedString("one")),
            EditorListItem(ListPrefix(indent: 1), AttributedString("sub")),
            EditorListItem(ListPrefix(ordered: true, number: 1), AttributedString("two")),
            EditorListItem(ListPrefix(indent: 3, checkbox: false), AttributedString("too deep")),
        ]
        #expect(EditorBlockContent.list(items).markdown == "1. one\n   - sub\n2. two\n   - [ ] too deep")
        let tens = (1...10).map { EditorListItem(ListPrefix(ordered: true, number: $0), AttributedString("i\($0)")) }
            + [EditorListItem(ListPrefix(indent: 1), AttributedString("under ten"))]
        #expect(EditorBlockContent.list(tens).markdown.hasSuffix("10. i10\n    - under ten"))
        #expect(EditorBlockContent.list([EditorListItem(.bullet)]).markdown == "- ")
    }

    @Test func utf16Splitting() {
        let t = AttributedString("a🎉b")
        #expect(t.utf16Count == 4)
        let (l, r) = t.split(utf16: 3)
        #expect(String(l.characters) == "a🎉" && String(r.characters) == "b")
        // an offset inside a surrogate pair never splits the character
        let (l2, _) = t.split(utf16: 2)
        #expect(String(l2.characters) == "a")
    }
}

@Suite(.timeLimit(.minutes(1))) struct EditorCommandTests {
    func p(_ s: String) -> EditorBlockContent { .paragraph(AttributedString(s)) }

    @Test func shortcutsAtTheStartOfABlock() {
        let cases: [(String, EditorBlockContent)] = [
            ("# ", .heading(level: 1, AttributedString())),
            ("## ", .heading(level: 2, AttributedString())),
            ("### ", .heading(level: 3, AttributedString())),
            ("- ", .list([EditorListItem(.bullet)])),
            ("* ", .list([EditorListItem(ListPrefix(marker: "*"))])),
            ("1. ", .list([EditorListItem(.numbered)])),
            ("4) ", .list([EditorListItem(ListPrefix(ordered: true, marker: ")", number: 4))])),
            ("[ ] ", .list([EditorListItem(.task)])),
            ("> ", .quote(AttributedString())),
        ]
        for (typed, want) in cases {
            let got = EditorCommands.shortcut(p(typed), caret: EditorCaret(offset: typed.utf16.count))
            #expect(got?.0 == want, "\(typed.debugDescription)")
            #expect(got?.1 == .start)
        }
        // typed in front of existing text, the text stays
        #expect(EditorCommands.shortcut(p("## title"), caret: EditorCaret(offset: 3))?.0 == .heading(level: 2, AttributedString("title")))
        // not at the caret, or not a trigger
        #expect(EditorCommands.shortcut(p("# "), caret: EditorCaret(offset: 1)) == nil)
        #expect(EditorCommands.shortcut(p("#tag "), caret: EditorCaret(offset: 5)) == nil)
        #expect(EditorCommands.shortcut(p("a - "), caret: EditorCaret(offset: 4)) == nil)
        #expect(EditorCommands.shortcut(.heading(level: 1, AttributedString("- ")), caret: EditorCaret(offset: 2)) == nil)
        // [ ] inside a bullet item makes it a task
        let item = EditorCommands.shortcut(.list([EditorListItem(.bullet, AttributedString("[ ] x"))]), caret: EditorCaret(offset: 4))
        #expect(item?.0 == .list([EditorListItem(.task, AttributedString("x"))]))
        // ``` then Return: a code block, caret on the empty middle line
        #expect(EditorCommands.returnKey(p("```swift"), at: EditorCaret(offset: 8)) == .update(.raw("```swift\n\n```"), caret: EditorCaret(offset: 9)))
    }

    @Test func returnInText() {
        #expect(EditorCommands.returnKey(p("hello world"), at: EditorCaret(offset: 5))
            == .split(current: p("hello"), inserted: [p(" world")], focus: 1, caret: .start))
        #expect(EditorCommands.returnKey(p("end"), at: EditorCaret(offset: 3))
            == .split(current: p("end"), inserted: [p("")], focus: 1, caret: .start))
        #expect(EditorCommands.returnKey(p("start"), at: .start) == .insertBefore(p("")))
        #expect(EditorCommands.returnKey(.heading(level: 2, AttributedString("Title")), at: EditorCaret(offset: 5))
            == .split(current: .heading(level: 2, AttributedString("Title")), inserted: [p("")], focus: 1, caret: .start))
        // a soft break at the cut goes away
        #expect(EditorCommands.returnKey(p("a\nb"), at: EditorCaret(offset: 1))
            == .split(current: p("a"), inserted: [p("b")], focus: 1, caret: .start))
        #expect(EditorCommands.returnKey(.raw("x"), at: .start) == .none)
    }

    @Test func returnInLists() {
        let list = EditorBlockContent.list([EditorListItem(.task, AttributedString("one")), EditorListItem(.task, AttributedString("two"))])
        #expect(EditorCommands.returnKey(list, at: EditorCaret(line: 0, offset: 2)) == .update(.list([
            EditorListItem(.task, AttributedString("on")), EditorListItem(.task, AttributedString("e")), EditorListItem(.task, AttributedString("two")),
        ]), caret: EditorCaret(line: 1, offset: 0)))
        // an empty last item leaves the list
        let trailing = EditorBlockContent.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(.bullet)])
        #expect(EditorCommands.returnKey(trailing, at: EditorCaret(line: 1, offset: 0))
            == .split(current: .list([EditorListItem(.bullet, AttributedString("a"))]), inserted: [p("")], focus: 1, caret: .start))
        // an empty middle item splits the list around a paragraph
        let middle = EditorBlockContent.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(.bullet), EditorListItem(.bullet, AttributedString("c"))])
        #expect(EditorCommands.returnKey(middle, at: EditorCaret(line: 1, offset: 0))
            == .split(current: .list([EditorListItem(.bullet, AttributedString("a"))]), inserted: [p(""), .list([EditorListItem(.bullet, AttributedString("c"))])], focus: 1, caret: .start))
        // an empty nested item outdents first
        let nested = EditorBlockContent.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(ListPrefix(indent: 1))])
        #expect(EditorCommands.returnKey(nested, at: EditorCaret(line: 1, offset: 0))
            == .update(.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(.bullet)]), caret: EditorCaret(line: 1, offset: 0)))
        // a numbered list continues its numbers
        let numbered = EditorBlockContent.list([EditorListItem(ListPrefix(ordered: true, number: 1), AttributedString("x"))])
        guard case let .update(.list(items), _) = EditorCommands.returnKey(numbered, at: EditorCaret(line: 0, offset: 1)) else {
            Issue.record("a new item")
            return
        }
        #expect(items.map(\.prefix.number) == [1, 2])
    }

    @Test func backspaceAtTheStart() {
        #expect(EditorCommands.backspaceAtStart(p("x"), at: .start) == .mergeWithPrevious)
        #expect(EditorCommands.backspaceAtStart(.heading(level: 1, AttributedString("h")), at: .start) == .update(p("h"), caret: .start))
        #expect(EditorCommands.backspaceAtStart(.quote(AttributedString("q")), at: .start) == .update(p("q"), caret: .start))
        #expect(EditorCommands.backspaceAtStart(.raw("code"), at: .start) == .none)
        #expect(EditorCommands.backspaceAtStart(.raw(""), at: .start) == .mergeWithPrevious)
        // a list item turns into a paragraph, splitting the list
        let list = EditorBlockContent.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(.bullet, AttributedString("b")), EditorListItem(.bullet, AttributedString("c"))])
        #expect(EditorCommands.backspaceAtStart(list, at: EditorCaret(line: 1, offset: 0)) == .split(
            current: .list([EditorListItem(.bullet, AttributedString("a"))]),
            inserted: [p("b"), .list([EditorListItem(.bullet, AttributedString("c"))])], focus: 1, caret: .start))
        #expect(EditorCommands.backspaceAtStart(list, at: EditorCaret(line: 0, offset: 0)) == .split(
            current: p("a"), inserted: [.list([EditorListItem(.bullet, AttributedString("b")), EditorListItem(.bullet, AttributedString("c"))])], focus: 0, caret: .start))
        // nested: outdent
        let nested = EditorBlockContent.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(ListPrefix(indent: 1), AttributedString("b"))])
        #expect(EditorCommands.backspaceAtStart(nested, at: EditorCaret(line: 1, offset: 0))
            == .update(.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(.bullet, AttributedString("b"))]), caret: EditorCaret(line: 1, offset: 0)))
    }

    @Test func mergeJoinsText() {
        #expect(EditorCommands.merge(p("ab"), p("cd"))?.0 == p("abcd"))
        #expect(EditorCommands.merge(p("ab"), p("cd"))?.1 == EditorCaret(offset: 2))
        #expect(EditorCommands.merge(.heading(level: 1, AttributedString("H")), p("x\ny"))?.0 == .heading(level: 1, AttributedString("Hx y")))
        let list = EditorBlockContent.list([EditorListItem(.bullet, AttributedString("a"))])
        #expect(EditorCommands.merge(list, p("b"))?.0 == .list([EditorListItem(.bullet, AttributedString("ab"))]))
        #expect(EditorCommands.merge(.raw("```\n```"), p("b")) == nil)
        #expect(EditorCommands.merge(p("a"), .raw("code")) == nil)
    }

    @Test func indentAndKinds() {
        let list = EditorBlockContent.list([EditorListItem(.bullet, AttributedString("a")), EditorListItem(.bullet, AttributedString("b")), EditorListItem(ListPrefix(indent: 1), AttributedString("c"))])
        // b and its child move in together
        #expect(EditorCommands.indent(list, at: EditorCaret(line: 1, offset: 0)) == .update(.list([
            EditorListItem(.bullet, AttributedString("a")), EditorListItem(ListPrefix(indent: 1), AttributedString("b")), EditorListItem(ListPrefix(indent: 2), AttributedString("c")),
        ]), caret: EditorCaret(line: 1, offset: 0)))
        #expect(EditorCommands.indent(list, at: EditorCaret(line: 0, offset: 0)) == .none, "the first item has nothing to nest under")
        #expect(EditorCommands.outdent(list, at: EditorCaret(line: 0, offset: 0)) == .none)
        #expect(EditorCommands.indent(p("x"), at: .start) == .none)

        let para = p("one\ntwo")
        #expect(EditorCommands.convert(para, to: .bullet, caret: EditorCaret(offset: 5))?.0
            == .list([EditorListItem(.bullet, AttributedString("one")), EditorListItem(.bullet, AttributedString("two"))]))
        #expect(EditorCommands.convert(para, to: .bullet, caret: EditorCaret(offset: 5))?.1 == EditorCaret(line: 1, offset: 1))
        #expect(EditorCommands.convert(para, to: .heading2, caret: .start)?.0 == .heading(level: 2, AttributedString("one two")))
        #expect(EditorCommands.convert(list, to: .paragraph, caret: EditorCaret(line: 1, offset: 1))?.1 == EditorCaret(offset: 3))
        #expect(EditorCommands.convert(.raw("x"), to: .paragraph, caret: .start) == nil)
        #expect(EditorCommands.toggleTodo(p("buy milk"), at: .start)?.0 == .list([EditorListItem(.task, AttributedString("buy milk"))]))
        #expect(EditorCommands.toggleTodo(.list([EditorListItem(.task, AttributedString("x"))]), at: .start)?.0 == .list([EditorListItem(.bullet, AttributedString("x"))]))
        #expect(EditorCommands.setChecked(.list([EditorListItem(.task, AttributedString("x"))]), item: 0, true).markdown == "- [x] x")
    }
}

@Suite(.timeLimit(.minutes(1))) struct WikiCompletionTests {
    let docs = [
        WikiCandidate(id: "1", title: "Roadmap", breadcrumb: "Grimoire"),
        WikiCandidate(id: "2", title: "Road trip notes"),
        WikiCandidate(id: "3", title: "Architecture", breadcrumb: "Grimoire"),
        WikiCandidate(id: "4", title: "Broad strokes"),
        WikiCandidate(id: "5", title: "Roadmap", breadcrumb: "Qompass"),
        WikiCandidate(id: "6", title: "Réunion agenda"),
        WikiCandidate(id: "7", title: "iOS app", breadcrumb: "Grimoire › Clients"),
    ]

    @Test func queryAtTheCaret() {
        #expect(WikiCompletion.query(before: "see [[Roa") == "Roa")
        #expect(WikiCompletion.query(before: "see [[") == "")
        #expect(WikiCompletion.query(before: "see [[Done]] and") == nil)
        #expect(WikiCompletion.query(before: "no link") == nil)
        #expect(WikiCompletion.query(before: "[[a\nb") == nil)
    }

    @Test func ranking() {
        func titles(_ q: String) -> [String] { WikiCompletion.rank(q, in: docs).map { "\($0.title)@\($0.breadcrumb ?? "")" } }
        #expect(titles("roadmap").prefix(2) == ["Roadmap@Grimoire", "Roadmap@Qompass"], "exact first")
        #expect(titles("road").first?.hasPrefix("Roadmap") == true, "prefix before word-inside")
        #expect(titles("road").contains("Broad strokes@"), "substring still matches")
        #expect(titles("road").prefix(3).allSatisfy { $0.hasPrefix("Road") }, "but after every title starting with it")
        #expect(titles("reunion").first == "Réunion agenda@", "diacritics fold")
        #expect(titles("qompass road").first == "Roadmap@Qompass", "breadcrumb words narrow it")
        #expect(titles("grimoire/arch").first == "Architecture@Grimoire", "path form")
        #expect(titles("rdmp").first?.hasPrefix("Roadmap") == true, "letters in order")
        #expect(titles("zzz").isEmpty)
        #expect(WikiCompletion.rank("", in: docs, limit: 3).count == 3)
    }

    @Test func insertedTarget() {
        #expect(WikiCompletion.target(for: docs[0], among: docs) == "Grimoire/Roadmap", "same title elsewhere: qualify")
        #expect(WikiCompletion.target(for: docs[2], among: docs) == "Architecture")
        #expect(WikiCompletion.target(for: docs[6], among: docs) == "iOS app")
    }
}
