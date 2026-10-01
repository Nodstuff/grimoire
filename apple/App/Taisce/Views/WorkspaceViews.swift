import SwiftUI
import TaisceKit

extension Color {
    /// `#rrggbb` (a workspace's colour); nil for anything else.
    init?(workspaceHex hex: String?) {
        guard let hex, hex.hasPrefix("#"), hex.count == 7, let v = UInt32(hex.dropFirst(), radix: 16) else { return nil }
        self.init(red: Double((v >> 16) & 0xff) / 255, green: Double((v >> 8) & 0xff) / 255, blue: Double(v & 0xff) / 255)
    }
}

struct WorkspaceDot: View {
    var color: String?
    var size: CGFloat = 8

    var body: some View {
        Circle()
            .fill(Color(workspaceHex: color) ?? Theme.secondary)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

/// The switcher pill: current workspace's dot and name; the menu lists the
/// others and "New workspace…".
struct WorkspaceSwitcher: View {
    let picker: WorkspacePicker
    var onSelect: (WorkspaceScope) -> Void = { _ in }
    var onNew: () -> Void = {}

    var body: some View {
        if let current = picker.current {
            Menu {
                ForEach(picker.options, id: \.self) { scope in
                    Button { onSelect(scope) } label: {
                        if scope == current {
                            Label(picker.name(scope), systemImage: "checkmark")
                        } else {
                            Text(picker.name(scope))
                        }
                    }
                }
                Divider()
                Button("New workspace\u{2026}", systemImage: "plus", action: onNew)
            } label: {
                HStack(spacing: 6) {
                    WorkspaceDot(color: picker.color(current))
                    Text(picker.name(current)).font(.caption.weight(.semibold)).foregroundStyle(Theme.text)
                    Image(systemName: "chevron.down").font(.caption2.weight(.semibold)).foregroundStyle(Theme.secondary)
                }
                .padding(.horizontal, 12)
                .frame(minHeight: 30)
                .background(Theme.surface2, in: .capsule)
                .frame(minHeight: Theme.minTarget)
                .contentShape(.rect)
            }
            .accessibilityLabel("Workspace: \(picker.name(current))")
        }
    }
}

/// The row above Today, Library and To-dos: the switcher, and (Library in
/// Unsorted) "Select" for triage.
struct WorkspaceBar: View {
    @Environment(AppModel.self) private var model
    var triage = false
    @State private var creating = false
    @State private var selecting = false

    var body: some View {
        if model.hasWorkspaces {
            HStack {
                WorkspaceSwitcher(picker: model.workspacePicker, onSelect: model.selectWorkspace, onNew: { creating = true })
                Spacer()
                if triage, model.currentWorkspace == .unsorted, !model.unsortedDocs.isEmpty {
                    Button("Select") { selecting = true }
                        .font(.caption.weight(.medium))
                        .foregroundStyle(Theme.accentActive)
                        .frame(minHeight: Theme.minTarget)
                }
            }
            .padding(.horizontal, Theme.gutter)
            .frame(maxWidth: Theme.readingWidth)
            .frame(maxWidth: .infinity)
            .background(Theme.ground)
            .sheet(isPresented: $creating) { NewWorkspaceSheet() }
            .sheet(isPresented: $selecting) { UnsortedTriageSheet() }
        }
    }
}

/// "New workspace…": a name and a colour.
struct NewWorkspaceSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var color: String? = WorkspacePalette.colors.first
    @State private var error: String?
    @State private var saving = false

    var body: some View {
        NavigationStack {
            Form {
                TextField("Name", text: $name).submitLabel(.done)
                Section("Colour") {
                    HStack(spacing: 12) {
                        ForEach(WorkspacePalette.colors, id: \.self) { c in
                            Button { color = c } label: {
                                WorkspaceDot(color: c, size: 26)
                                    .overlay(Circle().stroke(Theme.text, lineWidth: color == c ? 2 : 0).padding(-3))
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel(c)
                            .accessibilityAddTraits(color == c ? .isSelected : [])
                        }
                    }
                    .padding(.vertical, 6)
                }
                if let error {
                    Text(error).font(.footnote).foregroundStyle(Theme.rose)
                }
            }
            .navigationTitle("New workspace")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Create") {
                        saving = true
                        Task {
                            error = await model.createWorkspace(name: name, color: color)
                            saving = false
                            if error == nil { dismiss() }
                        }
                    }
                    .disabled(saving || name.trimmingCharacters(in: .whitespaces).isEmpty)
                }
            }
        }
        .presentationDetents([.medium])
    }
}

/// "Move to workspace…": the targets, Unsorted last (it unlabels).
struct MoveToWorkspaceSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let docIDs: [DocID]
    @State private var creating = false

    var body: some View {
        let picker = model.workspacePicker
        NavigationStack {
            List {
                ForEach(picker.ordered) { w in
                    target(.id(w.id), picker: picker)
                }
                target(.unsorted, picker: picker)
                Button("New workspace\u{2026}", systemImage: "plus") { creating = true }
            }
            .navigationTitle(docIDs.count == 1 ? "Move to workspace" : "Move \(docIDs.count) docs")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } } }
            .sheet(isPresented: $creating) { NewWorkspaceSheet() }
        }
        .presentationDetents([.medium, .large])
    }

    private func target(_ scope: WorkspaceScope, picker: WorkspacePicker) -> some View {
        Button {
            Task { await model.moveDocs(docIDs, to: scope) }
            dismiss()
        } label: {
            HStack(spacing: 10) {
                WorkspaceDot(color: picker.color(scope), size: 10)
                Text(picker.name(scope)).foregroundStyle(Theme.text)
                Spacer()
                if docIDs.count == 1, model.index.byID[docIDs[0]].map({ WorkspaceScope($0.workspaceID) == scope }) ?? false {
                    Image(systemName: "checkmark").foregroundStyle(Theme.accent)
                }
            }
        }
    }
}

/// Unsorted triage: select many, then "Move to…".
struct UnsortedTriageSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var selection: Set<DocID> = []
    @State private var moving = false

    var body: some View {
        NavigationStack {
            List(model.unsortedDocs, selection: $selection) { doc in
                VStack(alignment: .leading, spacing: 2) {
                    Text(doc.title).font(.subheadline).foregroundStyle(Theme.text)
                    if let crumb = model.index.breadcrumb(of: doc.id) {
                        Text(crumb).font(.caption2).foregroundStyle(Theme.secondary).lineLimit(1)
                    }
                }
            }
            .environment(\.editMode, .constant(.active))
            .navigationTitle(selection.isEmpty ? "Unsorted" : "\(selection.count) selected")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Move to\u{2026}") { moving = true }.disabled(selection.isEmpty)
                }
            }
            .sheet(isPresented: $moving, onDismiss: { selection = selection.filter { id in model.unsortedDocs.contains { $0.id == id } } }) {
                MoveToWorkspaceSheet(docIDs: Array(selection).sorted())
            }
        }
    }
}

/// The doc header's chip.
struct WorkspaceChip: View {
    let badge: WorkspaceBadge

    var body: some View {
        HStack(spacing: 5) {
            WorkspaceDot(color: badge.color, size: 7)
            Text(badge.name).font(.caption2.weight(.medium)).foregroundStyle(Theme.secondary)
        }
        .padding(.horizontal, 8)
        .frame(minHeight: 22)
        .background(Theme.surface2, in: .capsule)
        .accessibilityElement(children: .combine)
        .accessibilityLabel("Workspace: \(badge.name)")
    }
}

extension EnvironmentValues {
    /// Library's long-press "Move to workspace…" (nil = not offered).
    @Entry var moveToWorkspace: ((DocID) -> Void)?
}

/// Hosts the move sheet for the rows below it.
struct WorkspaceMoveHost: ViewModifier {
    @Environment(AppModel.self) private var model
    @State private var moving: DocID?

    func body(content: Content) -> some View {
        content
            .environment(\.moveToWorkspace, model.hasWorkspaces ? { moving = $0 } : nil)
            .sheet(item: Binding(get: { moving.map(MovingDoc.init) }, set: { moving = $0?.id })) { m in
                MoveToWorkspaceSheet(docIDs: [m.id])
            }
    }

    private struct MovingDoc: Identifiable { var id: DocID }
}

/// Library's empty state in an empty workspace.
struct WorkspaceEmptyView: View {
    let state: WorkspaceEmptyState

    var body: some View {
        VStack(spacing: 0) {
            WorkspaceBar(triage: true)
            ScrollView {
                VStack(alignment: .leading, spacing: 16) {
                    ScreenHeader(title: "Library") { EmptyView() }
                    EmptyCard(icon: "folder", title: state.title, hint: state.hint)
                }
                .padding(.horizontal, Theme.gutter)
                .frame(maxWidth: Theme.readingWidth)
                .frame(maxWidth: .infinity)
            }
        }
        .groundBackground()
        .toolbarVisibility(.hidden, for: .navigationBar)
    }
}

#if DEBUG
#Preview("Workspace switcher") {
    VStack(alignment: .leading, spacing: 16) {
        WorkspaceSwitcher(picker: WorkspacePicker(
            workspaces: [Workspace(id: "w1", name: "Work", color: "#5b8def"), Workspace(id: "w2", name: "Home", color: "#3fb68b")],
            unsortedCount: 3, stored: .id("w1")
        ))
        WorkspaceChip(badge: WorkspaceBadge(name: "Work", color: "#5b8def"))
        EmptyCard(icon: "folder", title: WorkspaceEmptyState(name: "Home").title, hint: WorkspaceEmptyState(name: "Home").hint)
    }
    .padding()
    .groundBackground()
    .preferredColorScheme(.dark)
}
#endif
