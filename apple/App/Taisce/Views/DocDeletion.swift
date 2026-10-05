import SwiftUI
import TaisceKit

/// "Delete…" in a doc's context menu: whether it's offered for a doc, and
/// asking. The confirmation and the call live in `DocDeleteHost`.
struct DocDeletion {
    var canDelete: (DocID) -> Bool
    var ask: (DocID) -> Void
}

extension EnvironmentValues {
    /// nil = not offered (no host above the rows)
    @Entry var docDeletion: DocDeletion?
}

/// The confirmation's words: the doc, and how many go with it.
enum DocDeletePrompt {
    static func title(_ docTitle: String) -> String { "Delete \u{201C}\(docTitle)\u{201D}?" }

    static func message(below: Int) -> String {
        let with = switch below {
        case 0: ""
        case 1: " with the 1 doc under it"
        default: " with the \(below) docs under it"
        }
        return "It moves to the Trash\(with). You can restore it from the Trash in the web app."
    }
}

/// Hosts the delete confirmation for the rows below it.
struct DocDeleteHost: ViewModifier {
    @Environment(AppModel.self) private var model
    @State private var asking: DocID?
    @State private var failure: String?

    func body(content: Content) -> some View {
        content
            .environment(\.docDeletion, DocDeletion(
                // the server also wants the whole subtree writable; it says so if not
                canDelete: { model.access(for: $0).canEdit },
                ask: { asking = $0 }
            ))
            .alert(
                asking.map { DocDeletePrompt.title(model.index.byID[$0]?.title ?? "this doc") } ?? "",
                isPresented: Binding(get: { asking != nil }, set: { if !$0 { asking = nil } }),
                presenting: asking
            ) { id in
                Button("Delete", role: .destructive) {
                    Task { failure = await model.deleteDoc(id) }
                }
                Button("Cancel", role: .cancel) {}
            } message: { id in
                Text(DocDeletePrompt.message(below: model.subtree(of: id).count - 1))
            }
            .alert(
                "Couldn't delete",
                isPresented: Binding(get: { failure != nil }, set: { if !$0 { failure = nil } }),
                presenting: failure
            ) { _ in
                Button("OK", role: .cancel) {}
            } message: { Text($0) }
    }
}
