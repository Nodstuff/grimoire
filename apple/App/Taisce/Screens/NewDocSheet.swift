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
    /// the docs it may go in, sorted once (not on every keystroke)
    @State private var choices = NewDocFolders()
    @FocusState private var titleFocused: Bool

    var body: some View {
        NavigationStack {
            List {
                Section {
                    TextField("Title", text: $title)
                        .lapisFont(.docHeading2)
                        .focused($titleFocused)
                        .submitLabel(.done)
                        .onSubmit { Task { await create() } }
                        .accessibilityIdentifier("newdoc.title")
                    if let error {
                        Text(error).font(.caption).foregroundStyle(Color.lapis.red)
                    }
                }
                .listRowBackground(Color.lapis.card)
                Section {
                    if model.currentAccess.canCreate {
                        folderRow(nil, title: "Top level", crumb: nil)
                    }
                    ForEach(choices.matching(filter), id: \.id) { d in
                        folderRow(d.id, title: d.title, crumb: choices.crumbs[d.id] ?? nil)
                    }
                } header: {
                    Text("Put it in")
                }
                .listRowBackground(Color.lapis.card)
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
            // built when the sheet opens, and again only when the docs change
            .task(id: model.docs) {
                choices = NewDocFolders(model.docs, canCreate: { model.access(for: $0).canCreate }, breadcrumb: { model.index.breadcrumb(of: $0) })
            }
        }
    }

    func folderRow(_ id: DocID?, title: String, crumb: String?) -> some View {
        Button { parent = id } label: {
            HStack(spacing: 12) {
                Image(systemName: id == nil ? "tray" : "folder").foregroundStyle(Color.lapis.accent).frame(width: 24).accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title).font(.subheadline).foregroundStyle(Color.lapis.ink)
                    if let crumb { Text(crumb).font(.caption2).foregroundStyle(Color.lapis.ink3).lineLimit(1) }
                }
                Spacer()
                if parent == id {
                    Image(systemName: "checkmark").foregroundStyle(Color.lapis.accent).accessibilityHidden(true)
                }
            }
            .frame(minHeight: Lapis.Layout.minTarget)
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

/// Every doc a new one may go in (not one in a workspace you only view),
/// sorted by where it lives then title, with its path: worked out once
/// when the sheet opens. The filter then ranks titles and paths without
/// sorting or walking the tree again, and finds each hit by id.
struct NewDocFolders {
    private(set) var all: [DocInfo] = []
    private(set) var crumbs: [DocID: String?] = [:]
    private var candidates: [WikiCandidate] = []
    private var byID: [DocID: DocInfo] = [:]

    init() {}

    init(_ docs: [DocInfo], canCreate: (DocID) -> Bool, breadcrumb: (DocID) -> String?) {
        let open = docs.filter { canCreate($0.id) }
        var crumbs: [DocID: String?] = [:]
        for d in open { crumbs[d.id] = breadcrumb(d.id) }
        all = open.sorted { ((crumbs[$0.id] ?? nil) ?? "", $0.title) < ((crumbs[$1.id] ?? nil) ?? "", $1.title) }
        self.crumbs = crumbs
        candidates = all.map { WikiCandidate(id: $0.id, title: $0.title, breadcrumb: crumbs[$0.id] ?? nil) }
        byID = Dictionary(all.map { ($0.id, $0) }, uniquingKeysWith: { a, _ in a })
    }

    /// The filter's words against titles and paths, best first (50 at most); every doc when empty.
    func matching(_ filter: String) -> [DocInfo] {
        let q = filter.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return all }
        return WikiCompletion.rank(q, in: candidates, limit: 50).compactMap { byID[$0.id] }
    }
}
