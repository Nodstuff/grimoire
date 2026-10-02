import SwiftUI
import UIKit
import TaisceKit

/// Server, account and sync status.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    /// "N changes haven't been sent and will be lost." (nil = not asking)
    @State private var unsentPrompt: String?
    @State private var checkingSignOut = false

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
                    lastError: model.lastError,
                    syncError: model.syncError,
                    failedDocs: model.failedDocs.map { (model.index.byID[$0.key]?.title ?? $0.key, $0.value) }.sorted { $0.0 < $1.0 },
                    alerts: model.dueAlerts.status
                ),
                onSave: { url in Task { await model.setServerURL(url) } },
                onEnableAlerts: { Task { await model.dueAlerts.requestAuthorization() } },
                onSignOut: {
                    guard !checkingSignOut else { return }
                    checkingSignOut = true
                    Task {
                        // one last send; ask before wiping anything still queued
                        let unsent = await model.unsentChangesBeforeSignOut()
                        checkingSignOut = false
                        if let prompt = SignOutCheck.prompt(unsent: unsent) {
                            unsentPrompt = prompt
                        } else {
                            await signOut()
                        }
                    }
                },
                onDone: { dismiss() }
            )
            .task { await model.dueAlerts.refresh() }
            // a dialog, not window.confirm: works on iPhone and Mac Catalyst
            .confirmationDialog(
                unsentPrompt ?? "",
                isPresented: Binding(get: { unsentPrompt != nil }, set: { if !$0 { unsentPrompt = nil } }),
                titleVisibility: .visible
            ) {
                Button(SignOutCheck.confirm, role: .destructive) {
                    unsentPrompt = nil
                    Task { await signOut() }
                }
                Button(SignOutCheck.cancel, role: .cancel) { unsentPrompt = nil }
            }
        }
    }
}

extension SettingsScreen {
    private func signOut() async {
        await model.signOut()
        dismiss()
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
    var syncError: String?
    /// (doc title, why) for docs whose body couldn't be fetched
    var failedDocs: [(String, String)] = []
    var alerts: DueAlertStatus = .allowed
}

struct SettingsContent: View {
    let info: SettingsInfo
    var now: Date = .now
    var onSave: (String) -> Void = { _ in }
    var onEnableAlerts: () -> Void = {}
    @Environment(\.openURL) private var openURL
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
                if let error = info.syncError {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Sync error").foregroundStyle(Theme.rose)
                        Text(error).font(.footnote).foregroundStyle(Theme.secondary)
                    }
                }
                ForEach(info.failedDocs, id: \.0) { title, why in
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Couldn't fetch \u{201C}\(title)\u{201D}").foregroundStyle(Theme.amber)
                        Text(why).font(.footnote).foregroundStyle(Theme.secondary).lineLimit(3)
                    }
                }
            } header: {
                Text("Sync")
            } footer: {
                Text("Edits made offline wait in the outbox and send in order when the connection is back.")
            }
            .listRowBackground(Theme.surface)

            Section {
                let prompt = DueAlertPrompt(info.alerts)
                switch prompt {
                case .on:
                    LabeledContent(prompt.settingsTitle, value: "On")
                case .turnOn:
                    Button(prompt.settingsTitle, action: onEnableAlerts)
                case .openSettings:
                    Button(prompt.settingsTitle) {
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                    }
                }
            } header: {
                Text("Notifications")
            } footer: {
                Text("A nudge when a to-do's deadline arrives.")
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
        case .checking: "Checking the server"
        case .notRequired: "Local daemon, no sign-in"
        case .signedIn: "Signed in"
        case .signedOut: "Signed out"
        }
    }
}

#if DEBUG
#Preview("Settings") {
    NavigationStack {
        SettingsContent(info: SettingsInfo(
            serverURL: "https://taisce.null.ie", account: .signedIn, status: .live,
            lastSynced: PreviewData.ago(40), cursor: 1842, pending: 0, alerts: .notDetermined
        ), now: PreviewData.now)
    }
    .preferredColorScheme(.dark)
}

#Preview("Settings, offline, light") {
    NavigationStack {
        SettingsContent(info: SettingsInfo(
            serverURL: "http://127.0.0.1:7425", account: .notRequired, status: .waiting(retryIn: .seconds(8)),
            lastSynced: PreviewData.ago(3 * 3600), cursor: 977, pending: 3, lastError: "Could not connect to the server.",
            syncError: "The request timed out.", failedDocs: [("Roadmap", "http(status: 500)")]
        ), now: PreviewData.now)
    }
    .preferredColorScheme(.light)
}
#endif
