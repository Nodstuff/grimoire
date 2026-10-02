/**
 * The shape a source file parses into. Nothing here knows about geometry —
 * these types are a faithful record of what the author wrote, and no more.
 */
export const DIRECTIONS = [
    'above',
    'below',
    'left',
    'right',
    'above-left',
    'above-right',
    'below-left',
    'below-right',
];
export function isDirection(word) {
    return DIRECTIONS.includes(word);
}
/**
 * Which side of the target a `level with` shares. `center` is the plain form;
 * the rest are written in front of it, as in `top level with media`.
 */
export const SIDES = ['center', 'top', 'bottom', 'left', 'right'];
/** A side belongs to one axis, so an alignment never has to say which. */
export const SIDE_AXIS = {
    center: 'y',
    top: 'y',
    bottom: 'y',
    left: 'x',
    right: 'x',
};
/**
 * The nine points of a box anybody can name without measuring: the four
 * corners, the four side midpoints, and the center. One closed set, accepted
 * everywhere the language has a position — which is the rule that replaced a
 * scatter of one-position slots, each decided on its own and each a little
 * piece of the same expressiveness loss.
 *
 * Closed-and-meaningful is allowed where open-and-ordinal is not: these are
 * words a reader decodes, and a diagram written in them still moves correctly
 * when a box moves, which is the property the refusal of `x: 140` protects.
 *
 * A compound position is hyphenated and is one token, matching the diagonal
 * directions above. A *phrase* of separate keywords stays spaced (`level
 * with`); a compound *word* does not.
 *
 * Every midpoint carries `-center` rather than standing alone as `top` or
 * `left`. Two reasons, and the second is the binding one. A side midpoint reads
 * as "the bottom edge, centered along it", which is what the word says. And
 * `top`, `bottom`, `left` and `right` already name a *side* in this language —
 * an edge's `from:` and an alignment's `top level with` — so a bare `bottom`
 * would mean a side in one place and a point in another. `from: bottom` spreads
 * attachments along the side; `from: bottom-center` will pin one to the point.
 */
export const POSITIONS = [
    'top-left',
    'top-center',
    'top-right',
    'left-center',
    'center',
    'right-center',
    'bottom-left',
    'bottom-center',
    'bottom-right',
];
export function isPosition(word) {
    return POSITIONS.includes(word);
}
/**
 * A part of a node that a placement may name: its text, one of its four sides,
 * or one of its nine points. Written as a separate word after the node's name
 * — `hub text`, `server right`, `board top-right`.
 *
 * No noun is carried. `board right side` was considered and dropped, because
 * the language already tells a segment from a point by *spelling*: a bare
 * `top` names a side everywhere (`from: bottom`, `top level with`) and a
 * hyphenated `top-right` names a point, which is exactly why every midpoint
 * carries `-center` rather than standing alone. A noun would mark with a word
 * a distinction the hyphen already marks.
 *
 * The spaced form needs no reservation against a child called `text`: a
 * sibling is always written dotted (`right of server.mirror`), so `hub text`
 * spaced can never be `hub.text`. The dotted spelling was rejected because it
 * interferes with the dot's one meaning.
 */
export const PART_SIDES = ['top', 'bottom', 'left', 'right'];
export const PARTS = ['text', ...PART_SIDES, ...POSITIONS];
export function isPart(word) {
    return PARTS.includes(word);
}
/**
 * Which way "toward the box's center" points from a part. This is the whole of
 * what `inside` and `outside` mean, which is why neither needs a table of its
 * own: `inside` is this direction, `outside` is its opposite, and one rule
 * covers every part.
 *
 * `center` and `text` are absent on purpose. Neither is on the boundary, so
 * there is no direction toward the interior from them, and both are refused by
 * name where the shorthand is read.
 */
export const INWARD = {
    top: 'below',
    bottom: 'above',
    left: 'right',
    right: 'left',
    'top-left': 'below-right',
    'top-center': 'below',
    'top-right': 'below-left',
    'left-center': 'right',
    'right-center': 'left',
    'bottom-left': 'above-right',
    'bottom-center': 'above',
    'bottom-right': 'above-left',
};
/** The other way round, for `outside`. */
export const OPPOSITE = {
    above: 'below',
    below: 'above',
    left: 'right',
    right: 'left',
    'above-left': 'below-right',
    'above-right': 'below-left',
    'below-left': 'above-right',
    'below-right': 'above-left',
};
/** The parts `inside` and `outside` can be read from — everything on the boundary. */
export const BOUNDARY_PARTS = Object.keys(INWARD);
/**
 * The modifiers a placement understands, in brackets after its targets. Refused
 * by name when unrecognized, for the reason `DIAGRAM_KEYS` are: a modifier that
 * silently does nothing looks like a bug in the tool rather than a typo.
 */
export const PLACEMENT_KEYS = ['gap'];
/**
 * `contents: (widths: match, align: center)` — how a container's children sit
 * inside it. A container has a fill, a border, a title and its contents, and
 * this is the one key that points at the last of them.
 *
 * Two properties, independent, which is why they are bracketed modifiers of one
 * key rather than two keys side by side. The cross product has a live cell in
 * every corner — *equal widths, ranged left, under a long title* is the case
 * that proves it — so one token could never carry both. And they sit at
 * different levels: `contents:` is a property of the node, `widths:` a property
 * of the contents, which written as peers would read as two facts about the box
 * when they are one about the box and one about its children.
 *
 * It replaced `align: widths`, which was a size operation wearing an
 * alignment's name and had a one-element value set — a flag in a property's
 * clothes. With it gone, `align` means one thing everywhere: how a text's lines
 * range against each other.
 */
export const CONTENTS_KEYS = ['widths', 'align'];
/**
 * What `widths:` may say. `natural` is the default and is today's behavior.
 * `match` is what `align: widths` did — every child as wide as the widest.
 * `fill` is the whole content band, which is `match` wherever the contents set
 * the container's width and the better answer wherever the title wins.
 */
export const CONTENT_WIDTHS = ['natural', 'match', 'fill'];
/**
 * Where the block of contents sits when it is narrower than the band. `right`
 * is accepted although no diagram has yet wanted it: refusing it would give
 * `align:` a different value set depending on which bracket it is in, which is
 * the divergence this scheme exists to remove.
 */
export const CONTENT_ALIGNMENTS = ['left', 'center', 'right'];
/** How a placement reads back in the author's own words, for error messages. */
export function describePlacement(placement) {
    const targets = listTargets(placement.targets);
    if (placement.kind === 'on') {
        return `on ${targets}`;
    }
    if (placement.kind === 'align') {
        const side = placement.side === 'center' ? '' : `${placement.side} `;
        return `${side}level with ${targets}`;
    }
    const gap = placement.gap === undefined ? '' : ` (gap: ${placement.gap})`;
    // `inside` and `outside` take no `of`, and are quoted back as the author
    // wrote them — the derived direction is what they *mean*, not what they say.
    if (placement.written)
        return `${placement.written} ${targets}${gap}`;
    // "left of X" and "above X" are both good English; "above of X" is not.
    const joiner = placement.direction === 'above' || placement.direction === 'below' ? '' : 'of ';
    return `${placement.direction} ${joiner}${targets}${gap}`;
}
/** "borg", "hub text", "borg and bare" — as the author would write them. */
export function listTargets(targets) {
    const written = targets.map(nameTarget);
    if (written.length <= 1)
        return written[0] ?? '';
    return `${written.slice(0, -1).join(', ')} and ${written[written.length - 1]}`;
}
/** One target in the author's words: the node's name, and its part if it named one. */
export function nameTarget(target) {
    return target.part === undefined ? target.name : `${target.name} ${target.part}`;
}
/** How the axis of a passage is written, and what it means. */
export const PASSAGE_AXES = {
    // The gap you measure with a vertical ruler: one target above, one below. A
    // line running along it therefore travels horizontally, which is the reading
    // to watch out for — the word describes the gap, not the direction of travel.
    vertically: 'y',
    horizontally: 'x',
};
/** `vertically` or `horizontally`, from the axis it binds. */
export function describeAxis(axis) {
    return axis === 'y' ? 'vertically' : 'horizontally';
}
/**
 * What a text's brackets may say: `"Docker" (at: bottom-center, color: theme-muted)`.
 *
 * They are bracketed onto the text rather than written among the node's
 * attributes for the same reason a gap is bracketed onto its placement — they
 * modify that one thing, and the brackets make the scope visible instead of
 * positional. What is left at the top level is then about the node itself:
 * `shape`, `icon`, `fill`, `border`, `gap`, `overlap`, `style`.
 *
 * In a style, which has no string for a bracket to hang off, the bracket hangs
 * off the key instead: `style synced  text: (color: theme-muted)`.
 *
 * `at` and `align` are independent and neither implies the other. `at` is where
 * the block of text sits in the node — one of the nine named positions — and
 * `align` is how its lines range against each other once it is there.
 */
export const TEXT_KEYS = ['color', 'size', 'wrap', 'align', 'at'];
/**
 * What an edge's line takes in its bracket: `line: (path: square, pattern:
 * dashed)`. The line is a part in the sense the text is, so every property of
 * it goes in one bracket, as the text's do. `line: red` stays as the short form
 * of `line: (color: red)`, and both are stored under the bare `line` key.
 */
export const LINE_KEYS = ['color', 'path', 'corners', 'crossing', 'pattern', 'thickness'];
/**
 * The words each of the line's properties takes, the default first. Thickness
 * takes a plain number of pixels as well.
 */
export const LINE_VALUES = {
    path: ['curved', 'square', 'straight'],
    corners: ['sharp', 'rounded'],
    crossing: ['gap', 'none', 'arc', 'square'],
    pattern: ['solid', 'dashed', 'dotted', 'dash-dot'],
    thickness: ['normal', 'thin', 'thick'],
};
/**
 * What may be drawn at an end of a line. The `o` prefix is the outlined form of
 * the shape, one rule for every shape; `none` says there is definitely nothing,
 * where an end with no mark written leaves it to a style.
 */
export const MARKS = ['arrow', 'oarrow', 'dot', 'odot', 'diamond', 'odiamond', 'bar', 'none'];
/**
 * The glyphs that stand for a mark, by the end they are written at. A glyph
 * that points is spelled the way it points, so `<` is the arrow at the left end
 * and `>` the one at the right, which is what makes `<->` read. The diamonds
 * have none: nothing would explain itself, and `<>` is two arrows.
 */
export const MARK_GLYPHS = {
    from: { '<': 'arrow', '<|': 'oarrow', '*': 'dot', o: 'odot', '|': 'bar' },
    to: { '>': 'arrow', '|>': 'oarrow', '*': 'dot', o: 'odot', '|': 'bar' },
};
/**
 * The attributes a `diagram` statement understands. `text` takes a bracket,
 * `text: (color: …)`, and sets the text color of everything at once — the
 * theme has one text color, shared by nodes and edges, and this is that.
 */
export const DIAGRAM_KEYS = ['theme', 'background', 'text'];
export const DEFAULT_TARGETS = ['node', 'leaf', 'container', 'edge'];
/**
 * What each default may say. A default carries a style's vocabulary, less the
 * words that would contradict the kind it is written for:
 *
 * - A default names its kind, so it is strict where a style is permissive —
 *   `default edge  fill:` can only be a mistake.
 * - `badge:` gives a box a child, which makes it a container, so every leaf
 *   given one by `default leaf` or `default node` would stop being a leaf.
 * - `icon:` draws a picture, and a picture cannot hold children, so it is a
 *   leaf's word and not a container's or every node's.
 */
export const DEFAULT_KEYS = {
    node: ['style', 'shape', 'fill', 'border', 'text'],
    leaf: ['style', 'shape', 'icon', 'fill', 'border', 'text'],
    container: ['style', 'shape', 'badge', 'fill', 'border', 'text'],
    edge: ['style', 'line', 'text'],
};
/**
 * The attributes whose value is a color rather than text. A color is written
 * as the viewer will receive it and the renderer keeps no list of color words
 * of its own, so there is nothing to check a value *against* — but quoting is
 * the author saying "this is text", and an unquoted value cannot hold a space,
 * so prose has to be quoted to get in at all. Refusing a quoted color is
 * therefore the whole of what can be checked here, and it happens to be the
 * mistake people actually make: `subtext: "medium-fine"` reads as the text
 * that goes underneath, and was accepted and dropped in silence.
 */
export const COLOR_KEYS = [
    'fill',
    'border',
    'line',
    'background',
];
/**
 * A color attribute names the *part* it colors, and a part exists only on the
 * kinds that have one. A node has a border and text; a note and a glyph body are
 * text and nothing else; an edge is a line and its text.
 *
 * This table is what makes the words checkable. `border:` on a note is refused
 * by name rather than ignored — the same rule as an unknown `diagram` key, and
 * for the same reason: an attribute that silently does nothing looks like the
 * tool being broken.
 *
 * Each entry is written the way the author would write it, since the text's is
 * a bracket rather than a bare key, and this list is only ever quoted back.
 *
 * A style spanning kinds writes one key per kind — `border: #d2904e  line:
 * #d2904e` — since a style contributes a part only to the kinds that have it.
 * That is what replaced `stroke:`, which named no part and so could never be
 * wrong, and which is why a node's text had no word of its own until now.
 */
export const COLOR_PARTS = {
    shape: ['fill:', 'border:', 'text: (color: …)'],
    icon: ['text: (color: …)'],
    none: ['text: (color: …)'],
    edge: ['line:', 'text: (color: …)'],
};
/**
 * Every attribute each kind understands. An attribute a kind has no use for is
 * refused by name rather than dropped, the same rule as an unknown `diagram`
 * key, a `PLACEMENT_KEYS` modifier or a color part — and for the same reason,
 * which the color parts only closed one level down: a key that silently does
 * nothing looks like the tool being broken rather than like a typo.
 *
 * The color entries repeat `COLOR_PARTS` and must agree with it. They are
 * written out rather than spliced in because this table is the answer to "what
 * may I write here", and a reader of it should not have to assemble the list
 * from two places.
 *
 * The exclusions are the whole of what this table decides, and each is a place
 * the old silence hid something:
 *
 * - A node drawn as a picture, or with no body at all, takes no `fill:` or
 *   `border:`. There is no outline for either to reach.
 * - Neither of those takes `contents:` either, which says how a node's children
 *   sit, and neither may have any.
 * - `shape:` and `icon:` each name the body, so each appears only on the kind it
 *   makes. `shape:` is on `none` as well, because `shape: none` is how that kind
 *   is written in the first place.
 * - An edge takes no `gap:` or `overlap:`. Those are about where a box sits, and
 *   an edge is not placed — it joins two things that are.
 */
export const ATTR_KEYS = {
    shape: ['style', 'gap', 'overlap', 'contents', 'badge', 'deck', 'shape', 'fill', 'border', 'text', 'url'],
    icon: ['style', 'gap', 'overlap', 'icon', 'text', 'url'],
    none: ['style', 'gap', 'overlap', 'shape', 'text', 'url'],
    edge: ['style', 'from', 'to', 'from-mark', 'to-mark', 'line', 'text', 'url'],
};
/**
 * Every word that is an attribute *somewhere*, which is what separates a
 * misspelling from a key written on the wrong kind of thing. The two deserve
 * different errors: one has no remedy but the spelling, the other has a real
 * meaning somewhere else in the file.
 *
 * `DIAGRAM_KEYS` is in here so that `background:` on a node is understood to be
 * a real word in the wrong place — that mistake wants to be pointed at `fill:`,
 * not told the word does not exist.
 */
export const ALL_ATTR_KEYS = [
    ...new Set([...Object.values(ATTR_KEYS).flat(), ...DIAGRAM_KEYS]),
];
