/** A problem in the source, reported in the vocabulary of the source. */
export class SourceError extends Error {
    line;
    constructor(message, line) {
        super(message);
        this.line = line;
        this.name = 'SourceError';
    }
    /** `12: two placements for "server"` — the form the command-line tool prints. */
    format(file) {
        const where = file ? `${file}:${this.line}` : `line ${this.line}`;
        return `${where}: ${this.message}`;
    }
}
