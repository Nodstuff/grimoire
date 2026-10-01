// The only module that runs in a browser and nowhere else. It is compiled by
// tsconfig.element.json, the one build that sees the DOM's types.
/**
 * `<reladraw-diagram>`: reladraw source written straight into a web page and
 * drawn in its place as an SVG. Loading this module is the whole setup:
 *
 *   <script type="module" src="https://cdn.jsdelivr.net/npm/reladraw/dist/element.js"></script>
 *   <reladraw-diagram>
 *     node a "Hello"
 *     node b "World" right of a
 *     edge a -> b
 *   </reladraw-diagram>
 *
 * `theme="dark"` renders in a named theme, as the command line's `--theme` does.
 * Setting `.source` from a script redraws with new text.
 */
import { SourceError } from './errors.js';
import { compile } from './index.js';
import { THEME_NAMES, THEMES } from './themes.js';
export class ReladrawDiagram extends HTMLElement {
    static observedAttributes = ['theme'];
    #source;
    connectedCallback() {
        if (this.#source !== undefined) {
            this.#draw();
        }
        else if (document.readyState === 'loading') {
            // Only when the module was loaded `async`, or before the page's own
            // markup: the element is connected when its opening tag is parsed, and
            // the text inside it has not arrived yet.
            document.addEventListener('DOMContentLoaded', () => this.connectedCallback(), { once: true });
        }
        else {
            this.#source = dedent(this.textContent ?? '');
            this.#draw();
        }
    }
    attributeChangedCallback() {
        if (this.#source !== undefined)
            this.#draw();
    }
    /** The diagram's source, as read from the page or last set. */
    get source() {
        return this.#source ?? '';
    }
    set source(text) {
        this.#source = text;
        this.#draw();
    }
    #draw() {
        const name = this.getAttribute('theme');
        if (name !== null && THEMES[name] === undefined) {
            this.#fail(`there is no theme called "${name}" — the themes are ${THEME_NAMES.join(', ')}`);
            return;
        }
        try {
            this.innerHTML = compile(this.source, name === null ? {} : { theme: THEMES[name] });
        }
        catch (error) {
            if (!(error instanceof SourceError))
                throw error;
            this.#fail(error.format());
        }
    }
    /** Said in the page where the diagram would have been, and in the console. */
    #fail(message) {
        const pre = document.createElement('pre');
        pre.textContent = `reladraw: ${message}`;
        pre.style.color = 'crimson';
        this.replaceChildren(pre);
        console.error(`reladraw: ${message}`, this);
    }
}
/**
 * Source as written inside an indented page: the blank lines around it
 * dropped, and the indentation every line shares taken off, so a continuation
 * line is indented relative to its statement and nothing else.
 */
export function dedent(text) {
    const lines = text.replace(/^\s*\n|\s+$/g, '').split('\n');
    const indents = lines.filter((line) => line.trim() !== '').map((line) => /^[ \t]*/.exec(line)[0].length);
    const shared = Math.min(...indents, Infinity);
    return lines.map((line) => line.slice(Math.min(shared, /^[ \t]*/.exec(line)[0].length))).join('\n');
}
if (customElements.get('reladraw-diagram') === undefined) {
    customElements.define('reladraw-diagram', ReladrawDiagram);
}
