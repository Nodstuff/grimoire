import { COLOR_KEYS, DEFAULT_KEYS, DEFAULT_TARGETS, DIAGRAM_KEYS, DIRECTIONS, describePlacement, SIDE_AXIS, SIDES, PASSAGE_AXES, TEXT_KEYS, LINE_KEYS, LINE_VALUES, CONTENTS_KEYS, PLACEMENT_KEYS, BOUNDARY_PARTS, INWARD, MARKS, MARK_GLYPHS, OPPOSITE, isDirection, isPart, isPosition, listTargets, nameTarget, } from './ast.js';
import { SourceError } from './errors.js';
import { STATEMENT_KEYWORDS } from './grammar.js';
import { isAttrKey, OpenString, tokenizeLine } from './lexer.js';
import { THEME_COLORS, THEME_NAMES, THEMES } from './themes.js';
/**
 * Parse a whole source file. A statement starts on an unindented line and runs
 * on through any line that starts with whitespace; a blank line or the next
 * unindented one ends it. How much whitespace never matters, only whether
 * there is any, so two lines that look alike cannot mean different things.
 *
 * An indented comment stays inside the statement and an unindented one ends
 * it. An indented line with nothing open above it is refused rather than read
 * as a statement of its own: it is a stray indent or a deleted first line, and
 * both are worth hearing about.
 *
 * A `"""` string is the one thing that crosses these rules: the lines it spans
 * are its own, blank or not, and a line opening with one continues the
 * statement above without being indented.
 */
export function parse(source) {
    const statements = [];
    /** The statement being gathered, and the line it starts on. */
    let open = null;
    // Brackets may close on a later line, and the lexer only reads `)` as a
    // bracket while one is open, so the depth is carried from line to line.
    let depth = 0;
    const close = () => {
        if (open)
            statements.push(parseStatement(open.tokens, open.line));
        open = null;
        depth = 0;
    };
    const lines = source.split(/\r?\n/);
    for (let index = 0; index < lines.length; index++) {
        const lineNumber = index + 1;
        let line = lines[index];
        if (line.trim() === '') {
            close();
            continue;
        }
        // A line opening with `"""` continues the statement above without being
        // indented, because nothing else could start with one: no statement begins
        // with a string, so `icon rack` on one line and the SVG from the next is
        // unambiguous. The exception is that narrow on purpose — a statement that
        // could carry on through any line would turn a forgotten word into an error
        // about some other line.
        const indented = line[0] === ' ' || line[0] === '\t';
        const continues = indented || line.startsWith('"""');
        if (!continues)
            close();
        // A `"""` string may run on over the lines after it, blank ones included,
        // and every line it covers belongs to it rather than to the statement
        // structure above.
        let tokens;
        for (;;) {
            try {
                tokens = tokenizeLine(line, lineNumber, depth);
                break;
            }
            catch (error) {
                if (!(error instanceof OpenString) || index + 1 >= lines.length)
                    throw error;
                index += 1;
                line += '\n' + lines[index];
            }
        }
        if (tokens.length === 0)
            continue;
        if (!continues) {
            open = { tokens, line: lineNumber };
        }
        else {
            const first = tokens[0];
            if (!open) {
                throw new SourceError(indented
                    ? 'this line is indented, which continues the statement above, but there is none to ' +
                        'continue — remove the indentation to start a new statement'
                    : 'a """ string at the start of a line belongs to the statement above, but there is ' +
                        'none — write it after the words it goes with, such as `icon rack`', lineNumber);
            }
            if (!first.quoted && STATEMENT_KEYWORDS.includes(first.text)) {
                throw new SourceError(`\`${first.text}\` cannot continue the statement above — an indented line continues ` +
                    `the one before it, so remove the indentation to start a new statement`, lineNumber);
            }
            open.tokens.push(...tokens);
        }
        for (const token of tokens) {
            if (token.quoted)
                continue;
            if (token.text === '(')
                depth += 1;
            else if (token.text === ')')
                depth -= 1;
        }
    }
    close();
    return { statements };
}
/**
 * Find the line at fault for an error reported on the first line of a
 * continued statement. Everything after the parser reports a statement by the
 * line it starts on, which for a continued one is often not where the mistake
 * is, so this asks which line brings the error in: `run` is tried on the
 * source with the statement cut after each of its lines in turn, and the first
 * cut that fails with the same message names the line. A cut that fails
 * differently — a bracket not yet closed, a placement not yet written — is not
 * the culprit and is passed over. Lines keep their numbers, because what is cut
 * is blanked rather than removed.
 *
 * This costs a few extra runs, and only when there is an error to report.
 */
export function blame(source, error, run) {
    const lines = source.split(/\r?\n/);
    const start = error.line - 1;
    const first = lines[start];
    if (first === undefined || first.trim() === '' || first[0] === ' ' || first[0] === '\t')
        return error;
    let end = start;
    while (end + 1 < lines.length && lines[end + 1].trim() !== '' && /^[ \t]/.test(lines[end + 1]))
        end += 1;
    if (end === start)
        return error;
    for (let last = start; last < end; last++) {
        const cut = lines.map((line, index) => (index > last && index <= end ? '' : line)).join('\n');
        try {
            run(cut);
        }
        catch (partial) {
            if (partial instanceof SourceError && partial.message === error.message) {
                return last === start ? error : new SourceError(error.message, last + 1);
            }
        }
    }
    return new SourceError(error.message, end + 1);
}
function parseStatement(tokens, line) {
    const keyword = tokens[0];
    if (!keyword || keyword.quoted) {
        throw new SourceError('a statement must begin with a keyword', line);
    }
    // What a line break inside a node's or an edge's text should mean has not
    // been decided, so until it is a `"""` string is refused anywhere but the one
    // place it was made for, rather than drawn in some way that might change.
    if (keyword.text !== 'icon' && tokens.some((token) => token.triple)) {
        throw new SourceError('a """ string holds the SVG of an `icon` declaration and is not accepted anywhere else yet — ' +
            'use ordinary quotes here', line);
    }
    switch (keyword.text) {
        case 'icon':
            return parseIcon(tokens, line);
        case 'node':
            return parseNode(tokens, line);
        case 'edge':
            return parseEdge(tokens, line);
        case 'style':
            return parseStyle(tokens, line);
        case 'diagram':
            return parseDiagram(tokens, line);
        case 'default':
            return parseDefault(tokens, line);
        default:
            throw new SourceError(substitution(keyword.text, tokens), line);
    }
}
/**
 * The statement keywords that are not words in this language, and the word to
 * write instead. `box` and `link` were the keywords until 0.3.0; `rect` and
 * `arrow` never were, and are here because they are what somebody arriving from
 * another format types first.
 *
 * Refused by name with the substitution quoted, the same treatment `stroke:`
 * and `width:` get. A synonym was the other candidate and is refused for the
 * reasons in the design record: an alias is a variant every reader has to
 * learn, and the statement keyword would become the one place a misspelling
 * silently succeeds.
 */
const SUBSTITUTIONS = {
    box: 'node',
    rect: 'node',
    link: 'edge',
    arrow: 'edge',
};
/**
 * What to say about a word that opens no statement. A word this language once
 * used, or one another format uses, gets the replacement quoted back in the
 * author's own name for the thing; anything else has no remedy but its
 * spelling.
 */
function substitution(word, head) {
    // A note is not a kind of statement any more, and the reason is worth the
    // longer message: a keyword names a picture, and "note" names a use. Free
    // text is a brace caption, a title over a diagram or an aside, so the
    // picture it names is a node with no body — which is what to write.
    if (word === 'note') {
        return `reladraw has no \`note\` statement — a note is a node with no body, so try ` +
            `\`node ${rewrite(head)} shape: none\``;
    }
    // A statement until 0.4.0. It created nothing, only said more about a node
    // declared elsewhere, which is what an attribute on that node is for.
    if (word === 'deck') {
        const name = head[1] && !head[1].quoted ? head[1].text : '<name>';
        const texts = head.slice(2).map((token) => (token.quoted ? quoteOf(token.text) : token.text));
        return `reladraw has no \`deck\` statement — a deck is an attribute of the node, so write ` +
            `\`deck: ${texts.length > 0 ? texts.join(' ') : '"…"'}\` on \`node ${name}\``;
    }
    const replacement = SUBSTITUTIONS[word];
    if (replacement === undefined)
        return `unknown statement "${word}"`;
    const plural = replacement === 'node' ? 'nodes' : 'edges';
    // Quote the fix in the line the author actually wrote. `node parser` and
    // `edge a -> b` both say more than a placeholder does, and the whole head is
    // what makes the second of those readable.
    const rest = rewrite(head);
    const example = rest === '' ? '' : ` \u2014 try \`${replacement} ${rest}\``;
    return `reladraw calls these ${plural}, so there is no \`${word}\` statement${example}`;
}
/** Everything after the keyword, written back the way the author would type it. */
function rewrite(head) {
    return head
        .slice(1)
        .map((token) => (token.quoted ? quoteOf(token.text) : token.text))
        .join(' ');
}
/** The value as the author would have to write it back into a text. */
function quoteOf(text) {
    return `"${text.replace(/"/g, '\\"')}"`;
}
/**
 * Everything after a statement's positional head: its attributes and, on a
 * node, its placements, in whatever order they were written.
 *
 * The ordering rule that used to stand here — placements first, attributes
 * after — existed because a bare `gap:` written between two placements could
 * not be told from the node-wide default. Gaps went into brackets on their own
 * placement, so that ambiguity is gone and with it the reason for the rule. A
 * `key:` token can never open a placement and a placement never opens with one,
 * so the two interleave with nothing to resolve.
 */
function parseTail(tokens, start, line, subject, other) {
    const attrs = {};
    const placements = [];
    let i = start;
    while (i < tokens.length) {
        const token = tokens[i];
        if (isAttrKey(token)) {
            i = readAttr(tokens, i, attrs, line, subject);
            continue;
        }
        const taken = other?.(tokens, i);
        if (taken !== undefined) {
            i = taken;
            continue;
        }
        const read = readPlacement(tokens, i, line, subject);
        placements.push(read.placement);
        i = read.next;
    }
    return { attrs, placements };
}
/**
 * The attribute keys whose value is a bracket rather than a word, and what may
 * be written inside it. `text:` is a style's way of saying what a node says in
 * the brackets after its own string; `contents:` names a part whose two
 * properties are independent and sit one level below the node.
 */
const BRACKET_KEYS = {
    text: TEXT_KEYS,
    contents: CONTENTS_KEYS,
    line: LINE_KEYS,
};
/** How each bracketed key's error quotes itself back, and what it is about. */
const BRACKET_ABOUT = {
    text: { kind: 'a text', example: 'color: theme-muted' },
    contents: { kind: 'a `contents:` bracket', example: 'widths: match' },
    line: { kind: 'a line', example: 'path: square' },
};
/**
 * Which of the line's properties a word belongs to, so that `line: square` —
 * the color key given a shape — can be pointed at the bracket.
 */
function linePropertyOf(word) {
    for (const [property, words] of Object.entries(LINE_VALUES)) {
        if (words.includes(word) && word !== 'normal')
            return property;
    }
    return undefined;
}
/**
 * The top-level keys that moved into the text's bracket in 0.3.0, and the
 * substitution each one gets. They are properties of a node's *text* and never
 * of the node, and leaving them at the top level is what let `size:` sit beside
 * `fill:` as though the two were the same sort of statement.
 */
const MOVED_INTO_BRACKET = ['size', 'wrap', 'align'];
/**
 * Store one attribute, refusing a key the line has already set. Two words on
 * one line are equally explicit, so nothing says which was meant — and keeping
 * either drops the other in silence. It is almost always an edit that forgot to
 * delete the old value, so the error shows both and asks for one.
 */
function setOnce(attrs, key, value, subject, line, shown = { key, value: (v) => v }) {
    const had = attrs[key];
    if (had !== undefined) {
        throw new SourceError(`${subject}: "${shown.key}" is written twice (${shown.value(had)}, ${shown.value(value)}) — keep one`, line);
    }
    attrs[key] = value;
}
/** Read one `key: value` pair, and refuse the words that used to be keys. */
function readAttr(tokens, at, attrs, line, subject) {
    const keyToken = tokens[at];
    const key = keyToken.text.slice(0, -1);
    const bracketKeys = BRACKET_KEYS[key];
    if (bracketKeys !== undefined && follows(tokens, at + 1, '(')) {
        // `text: (color: theme-muted)` — the whole bracket belongs to one part, and it is
        // stored under dotted keys so that a style merges into a node exactly the
        // way every other attribute does.
        const read = readBracket(tokens, at + 1, bracketKeys, {
            subject,
            what: `\`${key}:\``,
            kind: BRACKET_ABOUT[key].kind,
            example: BRACKET_ABOUT[key].example,
            line,
        });
        if (Object.keys(read.values).length === 0) {
            throw new SourceError(`${subject}: \`${key}:\` opens empty brackets`, line);
        }
        // Two brackets for one part are fine as long as they say different things;
        // the same property in both is the same defect as `fill:` written twice.
        for (const [inner, value] of Object.entries(read.values)) {
            // A line's color is stored where `line: red` puts it, so the short form
            // and the bracket are one key: a style's `line: red` merges under an
            // edge's `line: (color: blue)`, and writing both on one line is a repeat.
            const stored = key === 'line' && inner === 'color' ? 'line' : `${key}.${inner}`;
            setOnce(attrs, stored, value, subject, line, { key: `${key}: (${inner}: …)`, value: (v) => v });
        }
        return read.next;
    }
    if (key === 'url') {
        // `url: https://example.com` loses everything from the `//` onwards, because
        // `//` opens a comment — so the value is either missing entirely or is the
        // bare scheme, which reads as another attribute key. Neither report says
        // what is wrong, and the remedy is punctuation rather than a missing word.
        const value = tokens[at + 1];
        if (!value || !value.quoted) {
            throw new SourceError(`${subject}: a url is written in quotes — \`url: "https://example.com"\`. Without them ` +
                'everything from the `//` onwards is read as a comment', line);
        }
        setOnce(attrs, key, value.text, subject, line, { key, value: quoteOf });
        return at + 2;
    }
    if (key === 'style') {
        // `style: base, critical and alarm` — several bundles, applied in the order
        // written, a later one winning where two set the same key. The list reads
        // the way a placement's targets do: commas and `and` both separate. Stored
        // joined on a space, which no style name can hold.
        const names = [];
        let next = at + 1;
        for (;;) {
            const token = tokens[next];
            if (!token || token.quoted || isAttrKey(token) || token.text === '(' || token.text === ')') {
                throw new SourceError(names.length === 0 ? `attribute "style" has no value` : `${subject}: "style:" ends its list with a comma`, line);
            }
            const listed = token.text.endsWith(',') && token.text.length > 1;
            const name = listed ? token.text.slice(0, -1) : token.text;
            if (names.includes(name)) {
                throw new SourceError(`${subject}: style "${name}" is named twice in one \`style:\` — keep one`, line);
            }
            names.push(name);
            next += 1;
            if (follows(tokens, next, 'and')) {
                next += 1;
                continue;
            }
            if (listed)
                continue;
            break;
        }
        setOnce(attrs, key, names.join(' '), subject, line, { key, value: (v) => v.split(' ').join(', ') });
        return next;
    }
    if (key === 'deck') {
        // One quoted text per copy behind the node, back to front, as many as are
        // written. Stored joined on a line break, which no source line can hold.
        const texts = [];
        let next = at + 1;
        while (tokens[next]?.quoted)
            texts.push(tokens[next++].text);
        if (texts.length === 0) {
            const given = tokens[next];
            throw new SourceError(`${subject}: \`deck:\` takes one quoted text per copy behind the node, as in \`deck: "Drive 2" "Drive 3"\`` +
                (given && !isAttrKey(given) ? `, not \`deck: ${given.text}\`` : ''), line);
        }
        setOnce(attrs, key, texts.join('\n'), subject, line, {
            key,
            value: (v) => v.split('\n').map(quoteOf).join(' '),
        });
        return next;
    }
    if (bracketKeys !== undefined && key !== 'text' && key !== 'line') {
        // `contents: match` names the part and then says one of its two properties
        // without saying which. The brackets are what make the level shift visible,
        // so there is no unbracketed spelling to fall back to.
        const given = tokens[at + 1];
        throw new SourceError(`${subject}: \`${key}:\` takes its properties in brackets — write \`${key}: (${BRACKET_ABOUT[key].example})\`` +
            (given && !isAttrKey(given) ? `, not \`${key}: ${given.text}\`` : ''), line);
    }
    const valueToken = tokens[at + 1];
    if (!valueToken || isAttrKey(valueToken) || valueToken.text === ')') {
        throw new SourceError(`attribute "${key}" has no value`, line);
    }
    if (key === 'stroke') {
        // Removed 2026-09-09. It meant a different part on every kind — the
        // border of a box, the text of a note or a glyph body, the line of a
        // edge — so it could never be wrong, and a node's text had no word at all.
        // Refused by name rather than ignored: an older file must be told what
        // to write, not silently drawn without its colors.
        throw new SourceError('`stroke:` has been replaced by the part it colors — `border:` on a node, `text: (color: …)` on the text of anything, `line:` on an edge. A style shared between nodes and edges writes both, as in `border: #d2904e  line: #d2904e`', line);
    }
    if (key === 'width') {
        // Renamed 2026-09-09, and moved into the text's bracket in 0.3.0.
        throw new SourceError('`width:` is now `wrap:` and belongs to the text — it folds the text every n characters and says nothing about how wide anything is, so write it as `"…" (wrap: 30)`', line);
    }
    if (key === 'subtext') {
        // Removed in 0.3.0. It colored "every line after the first", which is a
        // positional slice: the rule lived in a style elsewhere in the file and was
        // applied by counting, so a reader of the text could not see it. Markup
        // says what is quiet where it is quiet, and reaches a word in the middle of
        // a line, which the slice never could.
        throw new SourceError('`subtext:` has been replaced by markup in the text — write `style dim  text: (color: theme-muted)` ' +
            'and mark the quiet words as `"Dropbox / [dim]synced[/dim]"`', line);
    }
    if (key === 'text') {
        // `text:` is the text's bracket now, so a bare word after it is either the
        // old color key or an attempt to set the words themselves. The quotes tell
        // the two apart, and they want different remedies.
        throw new SourceError(valueToken.quoted
            ? `\`text:\` is how a style says something about text, not how anything sets it — write the words in quotes after the name, as in \`node name ${quoteOf(valueToken.text)}\``
            : `\`text:\` takes the text's properties in brackets — write \`text: (color: ${valueToken.text})\` in a style, and \`(color: ${valueToken.text})\` in the brackets after a node's or an edge's own text`, line);
    }
    if (key === 'line' && !valueToken.quoted) {
        // `line: red` is the short form of the color, and the only one: every other
        // property of the line needs the bracket to say which it is.
        const property = linePropertyOf(valueToken.text) ?? (/^\d/.test(valueToken.text) ? 'thickness' : undefined);
        if (property !== undefined) {
            throw new SourceError(`\`line: ${valueToken.text}\` — \`line:\` on its own takes a color. The line's other properties go in its bracket: ` +
                `\`line: (${property}: ${valueToken.text})\``, line);
        }
    }
    if (key === 'align' && valueToken.text === 'widths') {
        // Removed in 0.3.0. It was a size operation wearing an alignment's name,
        // and its value set had one member — a flag in a property's clothes. Its
        // job is `contents: (widths: match)`, and with it gone `align` means one
        // thing everywhere.
        throw new SourceError('`align: widths` is now `contents: (widths: match)` — it is a size, not an alignment, and ' +
            'the same brackets take `align: center` for where the contents sit when the title is wider', line);
    }
    if (MOVED_INTO_BRACKET.includes(key)) {
        throw new SourceError(`\`${key}:\` belongs to the text rather than to the node — write it in the brackets after ` +
            `the text, as in \`"…" (${key}: ${valueToken.text})\`, or as \`text: (${key}: ${valueToken.text})\` in a style`, line);
    }
    if (valueToken.quoted && COLOR_KEYS.includes(key)) {
        // A quoted value is the author saying "this is text", and every one of
        // these keys takes a color. Without this the string is passed through as
        // a color, turns out not to be one, and nothing is drawn and nothing is
        // said.
        if (valueToken.text.startsWith('#')) {
            // A hex color that was merely quoted. The author wrote a color and the
            // remedy is punctuation, so say that rather than that it is not one.
            throw new SourceError(`a color is written without quotes — "${key}: ${valueToken.text}"`, line);
        }
        throw new SourceError(`"${key}" takes a color and a quoted value is text — drop the quotes if ${valueToken.text} is a color`, line);
    }
    if (COLOR_KEYS.includes(key))
        checkColorWord(valueToken.text, subject, line);
    setOnce(attrs, key, valueToken.text, subject, line);
    return at + 2;
}
/** Attributes only, for the statements that take no placements. */
function attrsOnly(tokens, start, line, subject) {
    const attrs = {};
    let i = start;
    while (i < tokens.length) {
        const token = tokens[i];
        if (!isAttrKey(token)) {
            throw new SourceError(`${subject}: expected an attribute like "key: value", found "${token.text}"`, line);
        }
        i = readAttr(tokens, i, attrs, line, subject);
    }
    return attrs;
}
/** Everything a text takes, less the one word that needs a box to sit in. */
const EDGE_TEXT_KEYS = TEXT_KEYS.filter((key) => key !== 'at');
/**
 * `text:` is how a *style* says something about the text of whatever wears it,
 * because a style has no string of its own. A node and an edge do, so they say
 * it in the brackets after that string, and there is one spelling per place.
 */
function refuseTextKey(attrs, subject, where, line) {
    for (const key of Object.keys(attrs)) {
        if (!key.startsWith('text.'))
            continue;
        const inner = key.slice('text.'.length);
        throw new SourceError(`${subject}: \`text: (…)\` is how a style says it, having no text of its own. This has one, ` +
            `so write \`(${inner}: ${attrs[key]})\` in the brackets ${where}`, line);
    }
}
/**
 * `node <name> ["<text>"] [(<text modifiers>)] [<placement> ...]`
 *
 * The text is optional and the name stands in for it, because a bare `node a`
 * asking for an empty rectangle is a default nobody wants: the first lines
 * anybody types are `node a` and `node b right of a`, and they mean the two
 * boxes to say "a" and "b". `""` is how a box says it is deliberately blank —
 * an invisible container, a glyph body, a node that is nothing but its icon —
 * and every such box already writes it, so nothing that predates this changed
 * meaning. The syntax being added was a parse error before, which is what
 * makes it purely additive.
 *
 * A dotted name shows its last segment only. Containment is already drawn, so
 * `server.docker` reading "docker" says everything the whole path would.
 *
 * A name now has two jobs, so renaming a node can change the picture. That is
 * the price, and it is honest: a file that states no text is saying the name
 * is the text.
 */
function parseNode(head, line) {
    const name = requireName(head[1], 'node', line);
    const written = head[2];
    const textToken = written?.quoted ? written : undefined;
    // A bare word here is a text somebody forgot to quote far more often than
    // it is anything else, and `"Parser" is not a direction` would send them
    // looking in the wrong place.
    if (written && !textToken && !isAttrKey(written) && !startsPlacement(written) && written.text !== '(') {
        throw new SourceError(`node "${name}": a text is quoted — write "${written.text}" rather than ${written.text}`, line);
    }
    const text = textToken ? textToken.text : name.slice(name.lastIndexOf('.') + 1);
    const subject = `node "${name}"`;
    const bracket = readBracket(head, textToken ? 3 : 2, TEXT_KEYS, {
        subject,
        what: 'the text',
        kind: 'a text',
        example: 'at: bottom',
        line,
    });
    const tail = parseTail(head, bracket.next, line, subject);
    refuseTextKey(tail.attrs, subject, 'after the name', line);
    return {
        kind: 'node',
        name,
        text,
        statedText: textToken !== undefined,
        textAttrs: bracket.values,
        placements: tail.placements,
        attrs: tail.attrs,
        line,
    };
}
const NEEDS_ARROW = 'an edge needs a line between its endpoints: "->", "<-", "<->" or "--"';
/**
 * The arrow between an edge's two names: a mark, a shaft, a mark. The shaft is
 * `-` or `--`, and either mark may be left off, so `--` is a plain line and
 * `<->` an arrow at each end.
 *
 * A mark is a word or a glyph, bare or in `()` or `[]`, and it must touch the
 * dashes or be bracketed: `edge a -- dot b` would otherwise read like a line to
 * a node named `dot`. So a bracketed mark may stand apart, which is why the
 * arrow can span several tokens — `[dot] -- [arrow]` is three, and `(dot)` is
 * three on its own, since the lexer splits a bracket that opens a token.
 */
function readArrow(head, line) {
    let at = 2;
    const pieces = [];
    const bracketed = () => {
        const token = head[at];
        if (!token || token.quoted)
            return;
        if (token.text === '(' && head[at + 2]?.text === ')' && !head[at + 1].quoted) {
            pieces.push(`(${head[at + 1].text})`);
            at += 3;
        }
        else if (/^\[[^\]]*\]$/.test(token.text)) {
            pieces.push(token.text);
            at += 1;
        }
    };
    bracketed();
    const shaft = head[at];
    if (!shaft || shaft.quoted || !shaft.text.includes('-'))
        throw new SourceError(NEEDS_ARROW, line);
    pieces.push(shaft.text);
    at += 1;
    if (shaft.text.endsWith('-'))
        bracketed();
    const text = pieces.join('');
    const parts = /^([^-]*)(-+)([^-]*)$/.exec(text);
    if (!parts) {
        throw new SourceError(`"${text}" is not an arrow — the line is one run of dashes, with a mark at either end`, line);
    }
    if (parts[2].length > 2) {
        throw new SourceError(`"${text}" has ${parts[2].length} dashes — the line is "-" or "--"`, line);
    }
    const from = readMark(parts[1], 'from', text, line);
    const to = readMark(parts[3], 'to', text, line);
    return {
        text,
        marks: { ...(from !== undefined ? { from } : {}), ...(to !== undefined ? { to } : {}) },
        next: at,
    };
}
/** One end of an arrow, as written: a mark word or glyph, perhaps bracketed, or nothing. */
function readMark(written, end, arrow, line) {
    if (written === '')
        return undefined;
    const inner = /^\((.*)\)$/.exec(written)?.[1] ?? /^\[(.*)\]$/.exec(written)?.[1] ?? written;
    if (MARKS.includes(inner))
        return inner;
    const glyph = MARK_GLYPHS[end][inner];
    if (glyph !== undefined)
        return glyph;
    const other = end === 'from' ? 'to' : 'from';
    const turned = MARK_GLYPHS[other][inner];
    if (turned !== undefined) {
        // A pointing glyph written at the end it points away from: `>--` or `--<`.
        const right = Object.entries(MARK_GLYPHS[end]).find(([, mark]) => mark === turned)[0];
        throw new SourceError(`"${arrow}": "${inner}" points the other way — at the ${end === 'from' ? 'left' : 'right'} end ` +
            `the ${turned} is "${right}", or write the word "${turned}"`, line);
    }
    throw new SourceError(`"${arrow}": "${inner}" is not a mark. The marks are ${MARKS.join(', ')}, and the glyphs ` +
        `${Object.keys(MARK_GLYPHS.to).join(' ')} and ${Object.keys(MARK_GLYPHS.from).filter((g) => !(g in MARK_GLYPHS.to)).join(' ')}`, line);
}
/**
 * `edge <from> <arrow> <to> ["<text>"] [between <a> and <b>]`. The arrow is
 * read by `readArrow`.
 *
 * `from` and `to` are the order the names are written in, never the arrow's
 * direction: `a <- b` is `a` then `b` with an arrow at `a`. With a mark at each
 * end many lines have no direction at all, and the written order is the one
 * every edge has — so `from:` names the first name's side on every edge.
 */
function parseEdge(head, line) {
    const left = requireName(head[1], 'edge', line);
    const arrow = readArrow(head, line);
    const rightToken = head[arrow.next];
    if (!rightToken || rightToken.quoted) {
        throw new SourceError('an edge needs a node on the right of the arrow', line);
    }
    let at = arrow.next + 1;
    const stray = head[at];
    if (stray &&
        !stray.quoted &&
        !isAttrKey(stray) &&
        stray.text !== '(' &&
        stray.text !== 'between' &&
        !isDirection(stray.text) &&
        (MARKS.includes(rightToken.text) || Object.hasOwn(MARK_GLYPHS.to, rightToken.text))) {
        // `edge a -- dot b`: a mark standing apart, which reads as a node named
        // `dot`. Said outright rather than left to the "not a direction" error.
        throw new SourceError(`edge ${left} ${arrow.text} ${rightToken.text} ${stray.text}: a mark touches the dashes or is ` +
            `in brackets — write "${arrow.text}${rightToken.text}" or "${arrow.text} [${rightToken.text}]"`, line);
    }
    const textToken = head[at]?.quoted ? head[at] : undefined;
    if (textToken)
        at += 1;
    const subject = `edge ${left} ${arrow.text} ${rightToken.text}`;
    // An edge's text takes the same bracket a node's does, less `at:`: a node's
    // text sits somewhere in a box and an edge's rides at the middle of its line,
    // so there is no position to name until a diagram asks for one.
    const bracket = readBracket(head, at, EDGE_TEXT_KEYS, {
        subject,
        what: 'the text',
        kind: "an edge's text",
        example: 'color: theme-muted',
        line,
    });
    at = bracket.next;
    let between;
    // An edge is not placed, so its tail holds attributes and the one clause that
    // is neither: `between`, which says which gap the line travels down.
    const tail = parseTail(head, at, line, subject, (tokens, index) => {
        const word = tokens[index];
        if (word.quoted || word.text !== 'between')
            return undefined;
        if (between)
            throw new SourceError(`${subject}: "between" is written twice`, line);
        // A gap has two sides, so `between` takes exactly two targets rather than
        // the open list a placement takes. `right of a and b` means "clear of
        // both", and there is no matching reading of "pass between three things".
        const read = readTargets(tokens, index + 1, subject, 'between', line);
        if (read.targets.length !== 2) {
            throw new SourceError(`"between" takes two nodes, one for each side of the gap — found ${read.targets.length}`, line);
        }
        let next = read.next;
        // Two targets sitting diagonally have two gaps between them, and this is
        // the only way to say which. It is optional because most pairs have one.
        const trailing = tokens[next];
        const axis = trailing && !trailing.quoted ? PASSAGE_AXES[trailing.text] : undefined;
        if (axis !== undefined)
            next += 1;
        between = { targets: read.targets, ...(axis !== undefined ? { axis } : {}) };
        return next;
    });
    // An edge is not placed, so a direction on its line says which side of that
    // node the line passes. The other placements say where a thing *is*, and
    // have no reading for a line.
    const passes = [];
    for (const placement of tail.placements) {
        const written = describePlacement(placement);
        if (placement.kind !== 'offset' || placement.written !== undefined) {
            throw new SourceError(`${subject}: "${written}" places a node, and an edge is not placed — it joins two things ` +
                'that are. On an edge, above, below, left of and right of say which side of a node the ' +
                'line passes', line);
        }
        if (placement.gap !== undefined) {
            throw new SourceError(`${subject}: "${written}" gives a gap, and a gap is kept between nodes — the line passes ` +
                'as close as it reads clearly. Drop the brackets', line);
        }
        passes.push(placement);
    }
    if (passes.length > 0 && between) {
        throw new SourceError(`${subject}: "between" and "${describePlacement(passes[0])}" on one edge — an edge passing ` +
            'between two things already has a side of each, so say one or the other', line);
    }
    refuseTextKey(tail.attrs, subject, 'after the arrow', line);
    return {
        kind: 'edge',
        textAttrs: bracket.values,
        from: left,
        to: rightToken.text,
        arrow: arrow.text,
        marks: arrow.marks,
        ...(textToken ? { text: textToken.text } : {}),
        ...(between ? { between } : {}),
        ...(passes.length > 0 ? { passes } : {}),
        attrs: tail.attrs,
        line,
    };
}
/** `style <name> <attributes>` */
function parseStyle(head, line) {
    const name = requireName(head[1], 'style', line);
    const attrs = attrsOnly(head, 2, line, `style "${name}"`);
    if (Object.keys(attrs).length === 0) {
        throw new SourceError(`style "${name}" sets nothing`, line);
    }
    if (attrs['url'] !== undefined) {
        // A destination is content, not appearance. A style is a bundle worn by
        // many things, so a `url:` in one would point every node wearing it at the
        // same place — which is never what anybody means, and would be silent.
        throw new SourceError(`style "${name}" has a url. A destination is part of what a node says rather than how it ` +
            'looks, so it is written on the node or the edge itself', line);
    }
    return { kind: 'style', name, attrs, line };
}
/**
 * `icon <name> "<svg>…</svg>"`, or `icon <name> ./file.svg`. What the picture
 * is — pasted markup or a file to read — is worked out when the diagram is
 * resolved, since only the caller knows whether files can be read at all.
 */
function parseIcon(head, line) {
    const name = requireName(head[1], 'icon', line);
    const picture = head[2];
    if (picture === undefined || isAttrKey(picture)) {
        throw new SourceError(`icon "${name}" needs its picture: the SVG itself between """ marks, or the name of an .svg file`, line);
    }
    if (head.length > 3) {
        throw new SourceError(`icon "${name}" takes one picture and nothing else — "${head[3].text}" is left over`, line);
    }
    return { kind: 'icon', name, source: picture.text, pasted: picture.triple === true || picture.text.trim().startsWith('<'), line };
}
/**
 * `diagram <attributes>` — no name, because a file holds one diagram. Unknown
 * keys are refused rather than ignored: a misspelt diagram-wide setting that
 * silently does nothing is the kind of thing an author stares at for a while.
 */
function parseDiagram(head, line) {
    const attrs = attrsOnly(head, 1, line, 'diagram');
    if (Object.keys(attrs).length === 0) {
        throw new SourceError('diagram sets nothing', line);
    }
    for (const key of Object.keys(attrs)) {
        if (key.startsWith('text.')) {
            // The theme has one text color, shared by everything, and that is all
            // the diagram's text sets. A size or a wrap is about one kind of text.
            if (key !== 'text.color') {
                const inner = key.slice('text.'.length);
                throw new SourceError(`diagram text: sets only a color — \`${inner}\` is about one kind of text, so write ` +
                    `\`text: (${inner}: ${attrs[key]})\` on a \`default node\` or \`default edge\``, line);
            }
            continue;
        }
        if (!DIAGRAM_KEYS.includes(key)) {
            throw new SourceError(`diagram has no "${key}" — it takes ${DIAGRAM_KEYS.join(', ')}`, line);
        }
    }
    if (attrs['text'] !== undefined) {
        throw new SourceError('diagram text: takes a bracket — `text: (color: #e0e0e0)`', line);
    }
    const theme = attrs['theme'];
    if (theme !== undefined && THEMES[theme] === undefined) {
        throw new SourceError(`there is no theme called "${theme}" — the themes are ${THEME_NAMES.join(', ')}`, line);
    }
    return { kind: 'diagram', attrs, line };
}
/**
 * `default <node | leaf | container | edge> <attributes>` — a style every thing
 * of that kind wears without naming it. It is strict about kind where a style
 * is permissive, because it names the kind it is for: a word that kind has no
 * use for can only be a mistake.
 */
function parseDefault(head, line) {
    const target = head[1];
    if (!target || target.quoted || !DEFAULT_TARGETS.includes(target.text)) {
        throw new SourceError(`default needs the kind it is for — ${DEFAULT_TARGETS.join(', ')}` +
            (target && !isAttrKey(target) ? `, not "${target.text}"` : ''), line);
    }
    const kind = target.text;
    const subject = `default ${kind}`;
    // `default leaf node` reads naturally and says nothing `default leaf` does not.
    const extra = head[2];
    if (extra && !extra.quoted && !isAttrKey(extra) && extra.text === 'node' && kind !== 'node') {
        throw new SourceError(`write \`default ${kind}\` — a ${kind} is already a node`, line);
    }
    const attrs = attrsOnly(head, 2, line, subject);
    if (Object.keys(attrs).length === 0) {
        throw new SourceError(`${subject} sets nothing`, line);
    }
    const allowed = DEFAULT_KEYS[kind];
    for (const key of new Set(Object.keys(attrs).map((k) => k.split('.')[0]))) {
        if (allowed.includes(key))
            continue;
        throw new SourceError(`${subject} has ${key}:, ${defaultRefusal(kind, key)}`, line);
    }
    return { kind: 'default', target: kind, attrs, line };
}
/** Why a default cannot carry a word, and what to write instead. */
function defaultRefusal(kind, key) {
    const takes = `it takes ${DEFAULT_KEYS[kind].map((k) => `${k}:`).join(', ')}`;
    if (key === 'url')
        return `and a destination belongs to one thing rather than to every one of a kind`;
    if (key === 'badge') {
        return 'and a badge is a child, so every leaf given one would become a container. ' +
            'Put the badge in a style, or write `default container  badge:`';
    }
    if (key === 'icon') {
        return 'and a picture cannot hold children, so an icon is a leaf\'s word — write `default leaf  icon:`';
    }
    if (kind === 'edge' && (key === 'fill' || key === 'border')) {
        return `which an edge does not have — an edge is colored by line: and text: (color: …)`;
    }
    if (kind !== 'edge' && key === 'line') {
        return `which is an edge's — a node is colored by fill:, border: and text: (color: …)`;
    }
    return `which a default does not set — ${takes}`;
}
function requireName(token, keyword, line) {
    if (!token || token.quoted) {
        throw new SourceError(`${keyword} needs a name`, line);
    }
    return token.text;
}
/**
 * Read one placement. A direction and a target (`right of docker`, `below
 * deploy`), an alignment (`level with docker`), or an overlay (`on hub at
 * top-right`).
 *
 * `of` is optional after every direction. "left of X" and "below X" are both
 * good English and "below of X" is not, so the word is accepted wherever it
 * helps and never demanded. Shorthands added later extend this without
 * disturbing what it already reads.
 */
function readPlacement(tokens, at, line, subject) {
    const word = tokens[at];
    if (word.quoted) {
        throw new SourceError(`${subject}: unexpected text "${word.text}"`, line);
    }
    if (word.text === 'on')
        return readOn(tokens, at, line, subject);
    if (word.text === 'inside' || word.text === 'outside') {
        return readTucked(tokens, at, line, subject, word.text);
    }
    // `top level with media` names a side rather than the center line. `left`
    // and `right` are sides as well as directions, so it is the word after them
    // that says which was meant — "left of drive" against "left level with drive".
    const side = isSideWord(word.text) && follows(tokens, at + 1, 'level') ? word.text : undefined;
    const head = side ? tokens[at + 1] : word;
    if (head.text === 'level' && !head.quoted) {
        const from = side ? at + 1 : at;
        const written = side ? `${side} level with` : 'level with';
        if (!follows(tokens, from + 1, 'with')) {
            throw new SourceError(`${subject}: an alignment reads "${written} <node>"`, line);
        }
        const read = readTargets(tokens, from + 2, subject, written, line);
        const modifiers = readModifiers(tokens, read.next, subject, `${written} ${listTargets(read.targets)}`, line);
        // An alignment shares a line outright, so there is no distance in it for
        // a gap to set. Refusing rather than dropping it, for the reason unknown
        // modifier names are refused: a word that quietly does nothing reads as a
        // fault in the tool.
        if (modifiers.gap !== undefined) {
            throw new SourceError(`${subject}: "${written} ${listTargets(read.targets)}" shares a line rather than leaving a space, so it takes no gap`, line);
        }
        return {
            placement: {
                kind: 'align',
                axis: SIDE_AXIS[side ?? 'center'],
                side: side ?? 'center',
                targets: read.targets,
                line,
            },
            next: modifiers.next,
        };
    }
    if (!isDirection(word.text)) {
        // A position word where a direction belongs is the one confusable pair, and
        // it is worth naming rather than only refusing: the two vocabularies reach
        // the same corner with different words and only one of them takes `of`.
        if (isPosition(word.text)) {
            throw new SourceError(`${subject}: "${word.text}" is a position on a box rather than a direction from one — ` +
                `write \`inside <node> ${word.text}\` to put this in that corner, \`on <node> ` +
                `${word.text}\` to straddle it, or a direction like ${DIRECTIONS.join(', ')} to put ` +
                'it outside', line);
        }
        throw new SourceError(`${subject}: "${word.text}" is not a direction`, line);
    }
    let next = at + 1;
    const of = follows(tokens, next, 'of');
    if (of)
        next += 1;
    const read = readTargets(tokens, next, subject, word.text, line);
    const modifiers = readModifiers(tokens, read.next, subject, `${word.text}${of ? ' of' : ''} ${listTargets(read.targets)}`, line);
    return {
        placement: {
            kind: 'offset',
            direction: word.text,
            targets: read.targets,
            ...(modifiers.gap !== undefined ? { gap: modifiers.gap } : {}),
            line,
        },
        next: modifiers.next,
    };
}
/**
 * `on hub top-right` — the node's center at the part's center, straddling it.
 *
 * One target, and the `and` list the other placements take is refused by name.
 * A direction against several targets means "clear of the box that bounds them
 * all", which is a floor and decomposes into one constraint per target; this
 * names an exact point of one box, and the box bounding two things is not a
 * box anybody drew.
 */
function readOn(tokens, at, line, subject) {
    const read = readTargets(tokens, at + 1, subject, 'on', line, true);
    const target = read.targets[0];
    if (read.targets.length > 1) {
        throw new SourceError(`${subject}: "on ${listTargets(read.targets)}" names ${read.targets.length} nodes, and a ` +
            'stamp sits on one box — name the one it is stamped on', line);
    }
    // `on X at <position>` shipped in 0.3.0 and never reached a release. Every
    // picture it drew is still drawable, in words that had to exist anyway, so
    // it is refused by name rather than left as a second spelling.
    if (follows(tokens, read.next, 'at')) {
        const wordToken = tokens[read.next + 1];
        const word = wordToken && !wordToken.quoted ? wordToken.text : '<position>';
        throw new SourceError(`${subject}: "on ${target.name} at ${word}" is no longer how a node is put on a box — ` +
            `write \`inside ${target.name} ${word}\` to tuck it inside that corner, or ` +
            `\`on ${target.name} ${word}\` to straddle it`, line);
    }
    const modifiers = readModifiers(tokens, read.next, subject, `on ${nameTarget(target)}`, line);
    if (modifiers.gap !== undefined) {
        throw new SourceError(`${subject}: "on ${nameTarget(target)}" puts this node's center on that point rather than ` +
            'leaving a space, so it takes no gap', line);
    }
    return {
        placement: { kind: 'on', targets: read.targets, line },
        next: modifiers.next,
    };
}
/**
 * `inside server right`, `outside board top-left` — a direction read off the
 * part rather than written.
 *
 * Both are shorthands, and their expansion is *derived* rather than listed:
 * inside is the direction from the named part toward the box's center, outside
 * is away from it. One rule covers every part — `inside right` is `left of`,
 * `inside top-right` is `below-left of` — so nobody writes a table and the
 * long form can be printed back.
 */
function readTucked(tokens, at, line, subject, written) {
    const read = readTargets(tokens, at + 1, subject, written, line, true);
    const target = read.targets[0];
    if (read.targets.length > 1) {
        throw new SourceError(`${subject}: "${written} ${listTargets(read.targets)}" names ${read.targets.length} nodes, ` +
            `and "${written}" reads its direction off one part of one box`, line);
    }
    const inward = target.part === undefined ? undefined : INWARD[target.part];
    if (inward === undefined) {
        const named = target.part === undefined
            ? `"${written} ${target.name}" names no part of "${target.name}"`
            : `"${written} ${nameTarget(target)}" reads no direction from "${target.part}", ` +
                'which is not on the boundary';
        throw new SourceError(`${subject}: ${named} — "${written}" takes a side or a point of the box: ` +
            `${BOUNDARY_PARTS.join(', ')}`, line);
    }
    const modifiers = readModifiers(tokens, read.next, subject, `${written} ${nameTarget(target)}`, line);
    return {
        placement: {
            kind: 'offset',
            direction: written === 'inside' ? inward : OPPOSITE[inward],
            written,
            targets: read.targets,
            ...(modifiers.gap !== undefined ? { gap: modifiers.gap } : {}),
            line,
        },
        next: modifiers.next,
    };
}
/**
 * The bracketed modifiers on one placement — `left of hub (gap: wide)`.
 *
 * A gap describes the relationship rather than the box at either end of it, so
 * a node wedged between two things can be tight against one and wide of the
 * other. The brackets are what make the scope visible: a bare `gap:` sitting
 * between two placements cannot be told from the node-wide default, and would
 * attach silently to whichever clause happened to precede it.
 */
function readModifiers(tokens, start, subject, placement, line) {
    const read = readBracket(tokens, start, PLACEMENT_KEYS, {
        subject,
        what: `"${placement}"`,
        kind: 'a placement',
        example: 'gap: wide',
        line,
    });
    return { ...read.values, next: read.next };
}
/**
 * A bracketed `key: value` list, shared by a placement's modifiers and a
 * text's. Both exist for the same reason — a modifier belongs to the clause it
 * modifies, and the brackets say which clause that is rather than leaving it to
 * be inferred from what happens to precede it.
 *
 * The keys are whitelisted and an unknown one is refused by name, the same rule
 * `DIAGRAM_KEYS` follows: a modifier that silently does nothing is worse than an
 * error, because the picture moves and nothing says why.
 */
function readBracket(tokens, start, keys, about) {
    if (!follows(tokens, start, '('))
        return { values: {}, next: start };
    const values = {};
    let i = start + 1;
    while (!follows(tokens, i, ')')) {
        const keyToken = tokens[i];
        if (!keyToken) {
            throw new SourceError(`${about.subject}: ${about.what} opens a "(" and never closes it`, about.line);
        }
        if (!isAttrKey(keyToken)) {
            throw new SourceError(`${about.subject}: ${about.what} takes modifiers like "${about.example}" in its brackets, found "${keyToken.text}"`, about.line);
        }
        const key = keyToken.text.slice(0, -1);
        if (!keys.includes(key)) {
            throw new SourceError(`${about.kind} has no "${key}" — it takes ${keys.join(', ')}`, about.line);
        }
        const valueToken = tokens[i + 1];
        if (!valueToken || valueToken.quoted || isAttrKey(valueToken) || valueToken.text === ')') {
            throw new SourceError(`${about.subject}: "${key}" has no value`, about.line);
        }
        // A comma between modifiers is punctuation, exactly as it is between the
        // targets of a placement. `(at: bottom, align: center)` and the same without
        // the comma are the same statement.
        const value = valueToken.text;
        const clean = value.endsWith(',') && value.length > 1 ? value.slice(0, -1) : value;
        if (key === 'color')
            checkColorWord(clean, about.subject, about.line);
        const had = values[key];
        if (had !== undefined) {
            throw new SourceError(`${about.subject}: "${key}" is written twice in the brackets after ${about.what} (${had}, ${clean}) — keep one`, about.line);
        }
        values[key] = clean;
        i += 2;
    }
    return { values, next: i + 1 };
}
/**
 * A color is any CSS color, which cannot be checked, or a `theme-` word, which
 * can. So a `theme-` word the theme does not define is refused rather than
 * handed to the viewer as a color that draws nothing, and `muted`, the one
 * theme word from before the prefix, is refused with its new name.
 */
function checkColorWord(value, subject, line) {
    if (value === 'muted') {
        throw new SourceError(`${subject}: \`muted\` is now \`theme-muted\` — every color that follows the theme begins \`theme-\``, line);
    }
    if (value.startsWith('theme-') && !THEME_COLORS.includes(value)) {
        throw new SourceError(`${subject}: there is no theme color "${value}" — the theme colors are ${THEME_COLORS.join(', ')}`, line);
    }
}
function follows(tokens, at, word) {
    const token = tokens[at];
    return token !== undefined && !token.quoted && token.text === word;
}
/**
 * Could this token open a placement? `top` and `left` open the side alignments,
 * `on` opens an overlay. Used only to tell a forgotten pair of quotes after a
 * node's name from a placement, so a word that is nearly one counts.
 */
function startsPlacement(token) {
    if (token.quoted)
        return false;
    return (isDirection(token.text) ||
        token.text === 'level' ||
        token.text === 'on' ||
        token.text === 'inside' ||
        token.text === 'outside' ||
        SIDES.includes(token.text));
}
function isSideWord(word) {
    return word !== 'center' && SIDES.includes(word);
}
/**
 * One target, or several joined by `and` — `right of borg and bare`, or
 * `level with borg, bare and media`. A trailing comma separates just as `and`
 * does, so both the way people write lists come out the same.
 *
 * Each name may be followed by a *part* of that node, spaced: `right of hub
 * text`, `inside server right`. See `partAfter` for the two words that are
 * parts everywhere else in the language too, and how they are told apart.
 * `partFirst` is for the placements where a side word after the name can only
 * be the part; see there.
 */
function readTargets(tokens, start, subject, placement, line, partFirst = false) {
    const targets = [];
    let i = start;
    for (;;) {
        const token = tokens[i];
        if (!token || token.quoted || token.text === '(' || token.text === ')') {
            throw new SourceError(`${subject}: "${placement}" names no node`, line);
        }
        const listed = token.text.endsWith(',') && token.text.length > 1;
        const name = listed ? token.text.slice(0, -1) : token.text;
        i += 1;
        // A part can only follow a name the author did not already close with a
        // comma — `a, b` is two targets and the comma says so.
        const part = listed ? undefined : partAfter(tokens, i, partFirst);
        if (part)
            i += 1;
        targets.push(part === undefined ? { name } : { name, part });
        if (follows(tokens, i, 'and')) {
            i += 1;
            continue;
        }
        if (listed)
            continue;
        return { targets, next: i };
    }
}
/**
 * The part word after a target's name, if there is one.
 *
 * Two of the part words are also the openings of something else, and both are
 * settled by the word that follows rather than by a reservation:
 *
 * - `right of hub right of mirror` — a side followed by `of` is the *direction*
 *   opening the next placement, which is how `left` and `right` have always
 *   been told apart.
 * - `right of hub top level with mirror` — a side followed by `level` is the
 *   alignment opening the next placement, the same lookahead `readPlacement`
 *   makes for `top level with`.
 *
 * The second does not hold after `inside`, `outside` or `on`, so `partFirst`
 * lifts it there: `inside server right level with server.db` is the right side
 * and then an alignment. `inside` and `outside` need a part, so a partless
 * reading is an error anyway; and `on` already fixes both axes, so an edge
 * alignment after a partless `on hub` would contradict it.
 */
function partAfter(tokens, at, partFirst = false) {
    const token = tokens[at];
    if (!token || token.quoted || !isPart(token.text))
        return undefined;
    if (follows(tokens, at + 1, 'of'))
        return undefined;
    if (!partFirst && follows(tokens, at + 1, 'level'))
        return undefined;
    return token.text;
}
