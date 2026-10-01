import SwiftUI
import TaisceKit

/// What an opened doc shows of its children (from the cached tree, never
/// filtered by workspace: an opened doc shows everything inside it).
enum DocChildrenLayout: Equatable {
    /// no children
    case none
    /// an empty body: the page IS the list of children
    case folder([DocCardModel])
    /// a body, then "Inside this doc"
    case section([DocCardModel])

    /// `pageEmpty` nil = still loading (children show once it settles).
    static func make(pageEmpty: Bool?, children: [DocCardModel]) -> DocChildrenLayout {
        guard !children.isEmpty, let pageEmpty else { return .none }
        return pageEmpty ? .folder(children) : .section(children)
    }

    /// The doc's children in tree order (sort key, then title).
    static func children(of id: DocID, index: DocIndex, meta: [DocID: EditMeta]) -> [DocCardModel] {
        index.docs
            .filter { $0.parentID == id && $0.id != id }
            .sorted { ($0.sortKey ?? "", $0.title) < ($1.sortKey ?? "", $1.title) }
            .map { DocCardModel(id: $0.id, title: $0.title, folder: index.breadcrumb(of: $0.id), edited: meta[$0.id]?.date) }
    }

    var cards: [DocCardModel] {
        switch self {
        case .none: []
        case .folder(let c), .section(let c): c
        }
    }
}

/// A child doc: serif title, breadcrumb, age, chevron.
struct ChildDocCard: View {
    let doc: DocCardModel
    var childCount = 0
    var now: Date = .now

    var body: some View {
        HStack(spacing: 12) {
            Image(systemName: childCount > 0 ? "folder" : "doc.text")
                .font(.subheadline)
                .foregroundStyle(childCount > 0 ? Theme.accent : Theme.secondary)
                .frame(width: 22)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(doc.title).font(Theme.serif(.headline)).foregroundStyle(Theme.text).lineLimit(2)
                if let sub = doc.subtitle(now: now) {
                    Text(sub).font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1)
                }
            }
            Spacer(minLength: 0)
            Image(systemName: "chevron.right").font(.caption.weight(.semibold)).foregroundStyle(Theme.secondary)
                .accessibilityHidden(true)
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 10)
        .frame(minHeight: Theme.minTarget)
        .contentShape(.rect)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isButton)
    }
}

/// The children of an opened doc: the whole page for a folder, a section
/// after the blocks otherwise.
struct DocChildrenList: View {
    let layout: DocChildrenLayout
    var childCounts: [DocID: Int] = [:]
    var now: Date = .now
    var onOpen: (DocID) -> Void = { _ in }
    var onNewDocHere: (() -> Void)?

    var body: some View {
        let cards = layout.cards
        if !cards.isEmpty {
            VStack(alignment: .leading, spacing: 10) {
                if case .section = layout {
                    SectionLabel("Inside this doc").padding(.top, 14)
                }
                if case .folder = layout, let onNewDocHere {
                    Button(action: onNewDocHere) {
                        Label("New doc here", systemImage: "plus")
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(Theme.accentActive)
                            .frame(minHeight: Theme.minTarget)
                    }
                    .buttonStyle(.plain)
                }
                CardList(items: cards) { doc in
                    Button { onOpen(doc.id) } label: { ChildDocCard(doc: doc, childCount: childCounts[doc.id] ?? 0, now: now) }
                        .buttonStyle(.plain)
                }
            }
        }
    }
}
