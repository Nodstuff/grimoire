import SwiftUI
import TaisceKit

/// "New doc": a title and a folder to put it in.
struct NewDocSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @Environment(\.dismiss) private var dismiss
    @State private var title = ""
    @State var parent: DocID?
    @State private var filter = ""
    @State private var creating = false
    @State private var error: String?
    @FocusState private var titleFocused: Bool

    var body: some View {
        NavigationStack {
            List {
                Section {
                    TextField("Title", text: $title)
                        .font(Theme.serif(.title3))
                        .focused($titleFocused)
                        .submitLabel(.done)
                        .onSubmit { Task { await create() } }
                        .accessibilityIdentifier("newdoc.title")
                    if let error {
                        Text(error).font(.caption).foregroundStyle(Theme.rose)
                    }
                }
                .listRowBackground(Theme.surface)
                Section {
                    folderRow(nil, title: "Top level", crumb: nil)
                    ForEach(folders, id: \.id) { d in
                        folderRow(d.id, title: d.title, crumb: model.index.breadcrumb(of: d.id))
                    }
                } header: {
                    Text("Put it in")
                }
                .listRowBackground(Theme.surface)
            }
            .listStyle(.insetGrouped)
            .groundBackground()
            .searchable(text: $filter, placement: .navigationBarDrawer(displayMode: .always), prompt: "Find a folder")
            .navigationTitle("New doc")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    if creating {
                        ProgressView()
                    } else {
                        Button("Create") { Task { await create() } }
                            .disabled(title.trimmingCharacters(in: .whitespaces).isEmpty)
                            .accessibilityIdentifier("newdoc.create")
                    }
                }
            }
            .onAppear { titleFocused = true }
        }
    }

    /// Every doc can hold docs; the filter matches titles and paths.
    var folders: [DocInfo] {
        let q = filter.trimmingCharacters(in: .whitespaces)
        let all = model.docs.sorted { (model.index.breadcrumb(of: $0.id) ?? "", $0.title) < (model.index.breadcrumb(of: $1.id) ?? "", $1.title) }
        guard !q.isEmpty else { return all }
        let candidates = all.map { WikiCandidate(id: $0.id, title: $0.title, breadcrumb: model.index.breadcrumb(of: $0.id)) }
        let ranked = WikiCompletion.rank(q, in: candidates, limit: 50).map(\.id)
        return ranked.compactMap { model.index.byID[$0] }
    }

    func folderRow(_ id: DocID?, title: String, crumb: String?) -> some View {
        Button { parent = id } label: {
            HStack(spacing: 12) {
                Image(systemName: id == nil ? "tray" : "folder").foregroundStyle(Theme.accent).frame(width: 24).accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.subheadline).foregroundStyle(Theme.text)
                    if let crumb { Text(crumb).font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1) }
                }
                Spacer()
                if parent == id {
                    Image(systemName: "checkmark").foregroundStyle(Theme.accent).accessibilityHidden(true)
                }
            }
            .frame(minHeight: Theme.minTarget)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityAddTraits(parent == id ? .isSelected : [])
    }

    func create() async {
        let t = title.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !t.isEmpty, !creating else { return }
        creating = true
        defer { creating = false }
        do {
            let id = try await model.createDoc(title: t, parent: parent)
            dismiss()
            router.open(.doc(id))
        } catch {
            self.error = "Couldn't create it: \(error.localizedDescription)"
        }
    }
}
