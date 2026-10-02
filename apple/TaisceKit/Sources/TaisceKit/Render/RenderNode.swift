import Foundation

/// A UI-neutral view of a block's markdown. Inline content stays as inline
/// markdown (`inline`) for the view layer to style (see `InlineMarkdown`).
public indirect enum RenderNode: Sendable, Hashable {
    case heading(level: Int, inline: String)
    case paragraph(inline: String)
    case list(ordered: Bool, start: Int, items: [ListItem])
    /// `> [!INFO]`-style callouts carry their kind; plain quotes have nil
    case quote(callout: String?, children: [RenderNode])
    case code(language: String?, code: String)
    case table(Table)
    /// mermaid, reladraw, vega-lite (drawn on the device), d2 (a labelled card)
    case diagram(kind: String, source: String)
    case frontmatter(String)
    case thematicBreak
    case html(String)

    public struct ListItem: Sendable, Hashable {
        /// nil = plain bullet; true/false = task-list checkbox
        public var checked: Bool?
        public var children: [RenderNode]
    }

    public struct Table: Sendable, Hashable {
        public enum Alignment: Sendable, Hashable { case leading, center, trailing }
        public var header: [String]
        public var rows: [[String]]
        public var alignments: [Alignment]
    }
}

/// A rendered block, ready for a list: identity + depth from the doc tree.
public struct RenderedBlock: Sendable, Hashable, Identifiable {
    public var id: BlockID
    public var depth: Int
    public var nodes: [RenderNode]
}
