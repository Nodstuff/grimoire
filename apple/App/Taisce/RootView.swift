import TaisceKit
import SwiftUI

enum SidebarItem: Hashable {
    case today
    case pinned
    case doc(DocID)
}

struct RootView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: SidebarItem? = .today
    @State private var query = ""
    @State private var showSettings = false

    var body: some View {
        if model.needsSignIn {
            SignInView()
        } else {
            docs
        }
    }

    private var docs: some View {
        NavigationSplitView {
            SidebarView(selection: $selection, query: query)
                .navigationTitle("Taisce")
                .searchable(text: $query, placement: .sidebar, prompt: "Search")
                .toolbar {
                    ToolbarItem {
                        Button("Settings", systemImage: "gear") { showSettings = true }
                    }
                }
        } detail: {
            NavigationStack {
                switch selection {
                case .today?: TodayView(selection: $selection)
                case .pinned?: ContentUnavailableView("Pinned docs", systemImage: "pin", description: Text("Coming soon."))
                case .doc(let id)?: DocView(docID: id, selection: $selection)
                case nil: ContentUnavailableView("Pick a doc", systemImage: "doc.text")
                }
            }
        }
        .sheet(isPresented: $showSettings) { SettingsView() }
    }
}
