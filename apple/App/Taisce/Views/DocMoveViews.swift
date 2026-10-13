import CoreTransferable
import SwiftUI
import TaisceKit

extension DocDrag: Transferable {
    static var transferRepresentation: some TransferRepresentation {
        ProxyRepresentation(exporting: \.token) { (token: String) in
            guard let drag = DocDrag(token: token) else { throw CocoaError(.coderReadCorrupt) }
            return drag
        }
    }
}

/// Moving docs in the tree from Library rows: drag one onto another (it
/// becomes that doc's last child), onto the Library header (the top
/// level), or "Move to…" in the context menu. nil = not offered.
struct DocMover {
    var canMove: (DocID) -> Bool
    /// "Move to…": the picker
    var ask: (DocID) -> Void
    /// a drop; false when it can't go there
    var drop: (DocID, DocID?) -> Bool
}

extension EnvironmentValues {
    @Entry var docMover: DocMover?
}

/// Hosts the "Move to…" picker and the refused-move alert for the rows below it.
struct DocMoveHost: ViewModifier {
    @Environment(AppModel.self) private var model
    @State private var moving: DocID?

    func body(content: Content) -> some View {
        @Bindable var model = model
        content
            .environment(\.docMover, DocMover(
                canMove: { model.canReparent($0) },
                ask: { moving = $0 },
                drop: { model.dropDoc($0, onto: $1) }
            ))
            .sheet(item: Binding(get: { moving.map(MovingDoc.init) }, set: { moving = $0?.id })) { m in
                MoveDocSheet(docID: m.id)
            }
            .alert(
                "Couldn't move",
                isPresented: Binding(get: { model.moveError != nil }, set: { if !$0 { model.moveError = nil } }),
                presenting: model.moveError
            ) { _ in
                Button("OK", role: .cancel) {}
            } message: { why in
                Text(why)
            }
    }

    private struct MovingDoc: Identifiable { var id: DocID }
}

/// "Move to…": every doc in the same workspace this one can go under,
/// and the top level. The non-drag path (touch, VoiceOver, keyboard).
struct MoveDocSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let docID: DocID
    @State private var query = ""

    var body: some View {
        let options = model.moveOptions(for: docID, query: query)
        NavigationStack {
            List {
                if options.isEmpty {
                    Text(query.isEmpty ? "Nowhere else to put it in this workspace." : "No docs match.")
                        .foregroundStyle(Theme.secondary)
                }
                ForEach(options) { option in
                    Button {
                        if !option.isCurrent { Task { await model.moveDoc(docID, into: option.parent) } }
                        dismiss()
                    } label: {
                        HStack(spacing: 12) {
                            Image(systemName: option.parent == nil ? "tray.full" : "folder")
                                .foregroundStyle(Theme.accent)
                                .frame(width: 24)
                                .accessibilityHidden(true)
                            VStack(alignment: .leading, spacing: 2) {
                                Text(option.title).font(.subheadline).foregroundStyle(Theme.text).lineLimit(2)
                                if let crumb = option.breadcrumb {
                                    Text(crumb).font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1)
                                }
                            }
                            Spacer(minLength: 0)
                            if option.isCurrent {
                                Image(systemName: "checkmark").foregroundStyle(Theme.accent).accessibilityLabel("Current place")
                            }
                        }
                        .frame(minHeight: Theme.minTarget)
                    }
                    .accessibilityHint(option.isCurrent ? "It's already here" : "Moves it here")
                }
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always), prompt: "Find a doc")
            .navigationTitle("Move \u{201C}\(model.index.byID[docID]?.title ?? "doc")\u{201D}")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
        }
        .presentationDetents([.medium, .large])
    }
}

extension View {
    /// A Library row you can drag (when you may move its doc) and drop
    /// another doc onto (it goes inside, as the last child).
    func docDragAndDrop(_ id: DocID, mover: DocMover?, onDroppedInside: @escaping () -> Void = {}) -> some View {
        modifier(DocDragDrop(id: id, mover: mover, onDroppedInside: onDroppedInside))
    }

    /// The top-level drop target (a Library header).
    func docTopLevelDrop(mover: DocMover?) -> some View {
        modifier(DocTopLevelDrop(mover: mover))
    }
}

private struct DocDragDrop: ViewModifier {
    let id: DocID
    let mover: DocMover?
    let onDroppedInside: () -> Void
    @State private var targeted = false

    func body(content: Content) -> some View {
        if let mover {
            dragged(content, mover: mover)
                .dropDestination(for: DocDrag.self) { items, _ in
                    guard let drag = items.first, drag.id != id, mover.drop(drag.id, id) else { return false }
                    onDroppedInside()
                    return true
                } isTargeted: { targeted = $0 }
                .background {
                    RoundedRectangle(cornerRadius: 8, style: .continuous)
                        .strokeBorder(Theme.accent, lineWidth: 2)
                        .opacity(targeted ? 1 : 0)
                        .padding(-4)
                        .allowsHitTesting(false)
                }
        } else {
            content
        }
    }

    @ViewBuilder private func dragged(_ content: Content, mover: DocMover) -> some View {
        if mover.canMove(id) {
            content.draggable(DocDrag(id: id))
        } else {
            content
        }
    }
}

private struct DocTopLevelDrop: ViewModifier {
    let mover: DocMover?
    @State private var targeted = false

    func body(content: Content) -> some View {
        if let mover {
            content
                .dropDestination(for: DocDrag.self) { items, _ in
                    guard let drag = items.first else { return false }
                    return mover.drop(drag.id, nil)
                } isTargeted: { targeted = $0 }
                .overlay(alignment: .bottomTrailing) {
                    if targeted {
                        Text("Move to the top level")
                            .font(.caption.weight(.medium))
                            .foregroundStyle(Theme.accent)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .background(Theme.surface2, in: .capsule)
                            .allowsHitTesting(false)
                    }
                }
        } else {
            content
        }
    }
}
