import SwiftUI
import TaisceKit

/// Server, account and sync status.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            SettingsContent(
                info: SettingsInfo(
                    serverURL: model.serverURL,
                    account: model.authPhase,
                    status: model.syncStatus,
                    lastSynced: model.lastSynced,
                    cursor: model.cursor,
                    pending: model.pendingWrites,
                    lastError: model.lastError
                ),
                onSave: { url in Task { await model.setServerURL(url) } },
                onSignOut: {
                    Task {
                        await model.signOut()
                        dismiss()
                    }
                },
                onDone: { dismiss() }
            )
        }
    }
}

struct SettingsInfo {
    var serverURL: String
    var account: AppModel.AuthPhase
    var status: SyncStatus
    var lastSynced: Date?
    var cursor: Int
    var pending: Int
    var lastError: String?
}

struct SettingsContent: View {
    let info: SettingsInfo
    var now: Date = .now
    var onSave: (String) -> Void = { _ in }
    var onSignOut: () -> Void = {}
    var onDone: () -> Void = {}
    @State private var url = ""

    var body: some View {
        Form {
            Section {
                TextField("Server URL", text: $url)
                    .textContentType(.URL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.body.monospaced())
                    .onSubmit { onSave(url) }
                if url != info.serverURL, !url.isEmpty {
                    Button("Connect to this server") { onSave(url) }
                }
            } header: {
                Text("Server")
            } footer: {
                Text("HTTPS servers ask you to sign in with a passkey. Plain HTTP is for a daemon on this device or network (no sign-in).")
            }
            .listRowBackground(Theme.surface)

            Section("Account") {
                LabeledContent("Status", value: accountText)
                if info.account == .signedIn {
                    Button("Sign out", role: .destructive, action: onSignOut)
                }
            }
            .listRowBackground(Theme.surface)

            Section {
                LabeledContent("Indicator") { SyncChip(badge: SyncBadge.make(status: info.status, pending: info.pending)) }
                LabeledContent("Connection", value: SyncBadge.describe(info.status))
                LabeledContent("Last synced", value: info.lastSynced.map { RelativeTime.string($0, now: now) } ?? "Not yet")
                LabeledContent("Cursor") { Text("seq \(info.cursor)").monospacedDigit() }
                LabeledContent("Pending writes") { Text("\(info.pending)").monospacedDigit() }
            } header: {
                Text("Sync")
            } footer: {
                Text("Edits made offline wait in the outbox and send in order when the connection is back.")
            }
            .listRowBackground(Theme.surface)

            if let error = info.lastError {
                Section("Last error") { Text(error).font(.footnote).foregroundStyle(Theme.secondary) }
                    .listRowBackground(Theme.surface)
            }
        }
        .groundBackground()
        .tint(Theme.accent)
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done", action: onDone)
            }
        }
        .onAppear { url = info.serverURL }
    }

    var accountText: String {
        switch info.account {
        case .notRequired: "Local daemon, no sign-in"
        case .signedIn: "Signed in"
        case .signedOut: "Signed out"
        }
    }
}

#Preview("Settings") {
    NavigationStack {
        SettingsContent(info: SettingsInfo(
            serverURL: "https://taisce.null.ie", account: .signedIn, status: .live,
            lastSynced: PreviewData.ago(40), cursor: 1842, pending: 0
        ), now: PreviewData.now)
    }
    .preferredColorScheme(.dark)
}

#Preview("Settings, offline, light") {
    NavigationStack {
        SettingsContent(info: SettingsInfo(
            serverURL: "http://127.0.0.1:7425", account: .notRequired, status: .waiting(retryIn: .seconds(8)),
            lastSynced: PreviewData.ago(3 * 3600), cursor: 977, pending: 3, lastError: "Could not connect to the server."
        ), now: PreviewData.now)
    }
    .preferredColorScheme(.light)
}
