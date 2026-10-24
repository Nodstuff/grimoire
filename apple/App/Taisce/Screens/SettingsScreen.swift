import SwiftUI
import UIKit
import TaisceKit

/// Settings: who you are (account, server, sync behind it), e-readers and
/// upload tokens (SERVER mode), appearance, notifications, shared links.
struct SettingsScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    /// "N changes haven't been sent and will be lost." (nil = not asking)
    @State private var unsentPrompt: String?
    @State private var checkingSignOut = false
    @State private var readers = ReaderDevicesStore()

    var body: some View {
        NavigationStack {
            SettingsContent(
                info: info,
                profileName: model.home.profileName,
                readers: readers,
                onSave: { url in Task { await model.setServerURL(url) } },
                onEnableAlerts: { Task { await model.dueAlerts.requestAuthorization() } },
                onSignOut: signOutTapped,
                onDone: { dismiss() },
                onCreate: { kind, label, stream in
                    Task { await readers.create(kind, label: label, stream: stream, api: model.api, server: model.serverURL, username: model.home.profileName) }
                },
                onRotate: { e in Task { await readers.rotateAndShow(e, api: model.api, server: model.serverURL, username: model.home.profileName) } },
                onRemove: { e in Task { await readers.removeReporting(e, api: model.api) } },
                onRevoke: { t in Task { await readers.revokeReporting(t, api: model.api) } }
            )
            .task { await model.dueAlerts.refresh() }
            .task { if model.shareLinks.isAvailable { try? await model.shareLinks.loadAll() } }
            .task { await model.home.loadProfile() }
            .task(id: model.authPhase) { await readers.load(api: model.authPhase == .signedIn ? model.api : nil) }
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

    private var info: SettingsInfo {
        SettingsInfo(
            serverURL: model.serverURL,
            account: model.authPhase,
            status: model.syncStatus,
            lastSynced: model.lastSynced,
            cursor: model.cursor,
            pending: model.pendingWrites,
            lastError: model.lastError,
            syncError: model.syncError,
            failedDocs: model.failedDocs.map { (model.index.byID[$0.key]?.title ?? $0.key, $0.value) }.sorted { $0.0 < $1.0 },
            alerts: model.dueAlerts.status,
            sharedLinks: model.shareLinks.isAvailable ? model.shareLinks.unreadTotal : nil
        )
    }

    private func signOutTapped() {
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
    /// unread link comments; nil hides Shared links (LOCAL mode, signed out)
    var sharedLinks: Int?

    /// "taisce.null.ie"
    var host: String { URL(string: serverURL)?.host() ?? serverURL }

    /// The line under your name.
    var accountLine: String {
        switch account {
        case .signedIn: "\(host) \u{00B7} signed in with a passkey"
        case .notRequired: "\(host) \u{00B7} local, no sign-in"
        case .checking: "\(host) \u{00B7} checking"
        case .signedOut: "\(host) \u{00B7} signed out"
        }
    }
}

struct SettingsContent: View {
    let info: SettingsInfo
    var profileName: String?
    /// nil in previews: the e-reader and token sections hide
    var readers: ReaderDevicesStore?
    var now: Date = .now
    var onSave: (String) -> Void = { _ in }
    var onEnableAlerts: () -> Void = {}
    @Environment(\.openURL) private var openURL
    var onSignOut: () -> Void = {}
    var onDone: () -> Void = {}
    var onCreate: (NewReaderDeviceSheet.Kind, String, Bool) -> Void = { _, _, _ in }
    var onRotate: (DevicePasswordReader) -> Void = { _ in }
    var onRemove: (DevicePasswordReader) -> Void = { _ in }
    var onRevoke: (UploadTokenInfo) -> Void = { _ in }
    @AppStorage(UserSettings.appearanceKey) private var appearance = LapisAppearance.system.rawValue
    @AppStorage(DocTextSize.key) private var textSize = DocTextSize.actual

    var body: some View {
        Form {
            Section {
                NavigationLink {
                    AccountSettingsScreen(info: info, now: now, onSave: onSave, onSignOut: onSignOut)
                } label: {
                    HStack(spacing: 14) {
                        LapisAvatar(name: profileName ?? "Taisce", agent: false, size: 48)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(profileName ?? "Account").font(.title3.weight(.semibold)).foregroundStyle(Color.lapis.ink)
                            Text(info.accountLine).font(.subheadline).foregroundStyle(Color.lapis.ink3).lineLimit(2)
                        }
                    }
                    .padding(.vertical, 6)
                }
                .accessibilityIdentifier("settings.account")
            }
            .listRowBackground(Color.lapis.card)

            if let readers, readers.phase != .unavailable {
                ReaderSections(readers: readers, now: now, onRotate: onRotate, onRemove: onRemove)
            }

            Section {
                Picker(selection: $appearance) {
                    ForEach(LapisAppearance.allCases) { a in
                        Label(a.title, systemImage: a.symbol).tag(a.rawValue)
                    }
                } label: {
                    Label { Text("Theme") } icon: { SettingsTile(symbol: "circle.lefthalf.filled", fill: Color.lapis.ink2) }
                }
                .pickerStyle(.menu)
                .accessibilityIdentifier("settings.appearance")
                .sensoryFeedback(.selection, trigger: appearance)
                #if targetEnvironment(macCatalyst)
                HStack(spacing: 12) {
                    SettingsTile(symbol: "textformat.size", fill: Color.lapis.ink2)
                    Text("A").lapisFont(.meta)
                    Slider(value: Binding(get: { Double(textSize) }, set: { textSize = Int($0.rounded()) }),
                           in: 0...Double(DocTextSize.steps.count - 1), step: 1)
                        .accessibilityLabel("Doc text size")
                    Text("A").lapisFont(.docHeading2)
                }
                .foregroundStyle(Color.lapis.ink2)
                #endif
            } header: {
                Text("Appearance")
            } footer: {
                #if targetEnvironment(macCatalyst)
                Text("System follows this Mac's light or dark setting. Text size is the doc's (\u{2318}+ and \u{2318}\u{2212}).")
                #else
                Text("System follows this device's light or dark setting. Text size follows iOS Settings \u{203A} Display & Brightness \u{203A} Text Size.")
                #endif
            }
            .listRowBackground(Color.lapis.card)

            Section {
                let prompt = DueAlertPrompt(info.alerts)
                switch prompt {
                case .on:
                    LabeledContent { Text("On") } label: {
                        Label { Text(prompt.settingsTitle) } icon: { SettingsTile(symbol: "bell.fill", fill: Color.lapis.red) }
                    }
                case .turnOn:
                    Button(action: onEnableAlerts) {
                        Label { Text(prompt.settingsTitle) } icon: { SettingsTile(symbol: "bell.fill", fill: Color.lapis.red) }
                    }
                case .openSettings:
                    Button {
                        if let url = URL(string: UIApplication.openSettingsURLString) { openURL(url) }
                    } label: {
                        Label { Text(prompt.settingsTitle) } icon: { SettingsTile(symbol: "bell.fill", fill: Color.lapis.red) }
                    }
                }
            } header: {
                Text("Notifications")
            } footer: {
                Text("A nudge when a to-do's deadline arrives.")
            }
            .listRowBackground(Color.lapis.card)

            if let unread = info.sharedLinks {
                Section {
                    NavigationLink {
                        SharedLinksScreen()
                    } label: {
                        HStack {
                            Label { Text("Shared links") } icon: { SettingsTile(symbol: "link", fill: Color.lapis.green) }
                            Spacer()
                            if unread > 0 { UnreadBadge(count: unread) }
                        }
                    }
                    .accessibilityIdentifier("settings.sharedLinks")
                } footer: {
                    Text("Links you've made to docs, their views and comments.")
                }
                .listRowBackground(Color.lapis.card)
            }

            #if targetEnvironment(macCatalyst)
            Section {
                NavigationLink {
                    DataSourcesScreen()
                } label: {
                    Label { Text("Data sources") } icon: { SettingsTile(symbol: "cylinder.split.1x2", fill: Color.lapis.ink2) }
                }
                .accessibilityIdentifier("settings.dataSources")
            } header: {
                Text("SQL blocks")
            } footer: {
                Text("Databases ```sql db=<name>``` blocks run on, on this Mac only.")
            }
            .listRowBackground(Color.lapis.card)
            #endif

            Section {
                NavigationLink {
                    AccountSettingsScreen(info: info, now: now, onSave: onSave, onSignOut: onSignOut)
                } label: {
                    LabeledContent {
                        SyncDotLabel(badge: SyncBadge.make(status: info.status, pending: info.pending))
                    } label: {
                        Label { Text("Sync") } icon: { SettingsTile(symbol: "arrow.triangle.2.circlepath", fill: Color.lapis.accent) }
                    }
                }
            } footer: {
                if let error = info.lastError {
                    Text("Last error: \(error)")
                }
            }
            .listRowBackground(Color.lapis.card)
        }
        .scrollContentBackground(.hidden)
        .groundBackground()
        .tint(Color.lapis.accent)
        .modifier(OptionalReaderPresenters(readers: readers, onCreate: onCreate, onRevoke: onRevoke))
        .navigationTitle("Settings")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .confirmationAction) {
                Button("Done", systemImage: "checkmark", action: onDone)
                    .accessibilityIdentifier("settings.done")
            }
        }
    }
}

/// E-readers and Upload Tokens: list with last use, add (the secret shown
/// once), remove or revoke with a confirmation. The sheets and dialogs
/// hang off the Form (`ReaderPresenters`), not a section.
private struct ReaderSections: View {
    let readers: ReaderDevicesStore
    var now: Date = .now
    let onRotate: (DevicePasswordReader) -> Void
    let onRemove: (DevicePasswordReader) -> Void

    var body: some View {
        Section {
            switch readers.phase {
            case .loading:
                SkeletonLines(count: 2).padding(.vertical, 6)
            case .failed(let why):
                Label(why, systemImage: "exclamationmark.triangle").font(.subheadline).foregroundStyle(Color.lapis.yellow)
            case .ready, .unavailable:
                ForEach(readers.ereaders) { e in
                    NavigationLink {
                        EreaderDetailScreen(ereader: e, onRotate: { onRotate(e) }, onRemove: { onRemove(e) })
                    } label: {
                        row(symbol: "book.closed.fill", fill: Color.lapis.accent, title: e.label,
                            sub: LastUsed.text(e.lastUsedAt, now: now) + (e.streamDownloads ? " \u{00B7} streams downloads" : ""))
                    }
                }
                Button { readers.adding = .ereader } label: {
                    Label("Add E-reader\u{2026}", systemImage: "plus")
                }
                .disabled(readers.busy)
                .accessibilityIdentifier("settings.addEreader")
            }
        } header: {
            Text("E-readers")
        } footer: {
            Text("Your books reach these over a catalog feed (OPDS); reading progress syncs back here.")
        }
        .listRowBackground(Color.lapis.card)

        if readers.phase == .ready {
            Section {
                ForEach(readers.tokens) { t in
                    row(symbol: "key.fill", fill: Color.lapis.agent, title: t.label, sub: LastUsed.text(t.lastUsedAt, now: now))
                        .swipeActions { Button("Revoke", role: .destructive) { readers.revoking = t } }
                        .contextMenu { Button("Revoke\u{2026}", systemImage: "trash", role: .destructive) { readers.revoking = t } }
                        .accessibilityAction(named: "Revoke") { readers.revoking = t }
                }
                Button { readers.adding = .token } label: {
                    Label("Create Upload Token\u{2026}", systemImage: "plus")
                }
                .disabled(readers.busy)
                .accessibilityIdentifier("settings.addToken")
            } header: {
                Text("Upload Tokens")
            } footer: {
                Text("Send EPUBs from a Shortcut or Calibre without signing in. A token can only add books.")
            }
            .listRowBackground(Color.lapis.card)
        }
    }

    private func row(symbol: String, fill: Color, title: String, sub: String) -> some View {
        HStack(spacing: 14) {
            SettingsTile(symbol: symbol, fill: fill)
            VStack(alignment: .leading, spacing: 2) {
                Text(title).foregroundStyle(Color.lapis.ink)
                Text(sub).font(.subheadline).foregroundStyle(Color.lapis.ink3)
            }
        }
        .padding(.vertical, 2)
        .accessibilityElement(children: .combine)
    }
}

/// `ReaderPresenters` when there is a store (previews have none).
private struct OptionalReaderPresenters: ViewModifier {
    let readers: ReaderDevicesStore?
    let onCreate: (NewReaderDeviceSheet.Kind, String, Bool) -> Void
    let onRevoke: (UploadTokenInfo) -> Void

    func body(content: Content) -> some View {
        if let readers {
            content.modifier(ReaderPresenters(readers: readers, onCreate: onCreate, onRevoke: onRevoke))
        } else {
            content
        }
    }
}

/// The e-reader and token sheets, the revoke confirmation and failures.
private struct ReaderPresenters: ViewModifier {
    let readers: ReaderDevicesStore
    let onCreate: (NewReaderDeviceSheet.Kind, String, Bool) -> Void
    let onRevoke: (UploadTokenInfo) -> Void

    private struct AddingKind: Identifiable {
        let kind: NewReaderDeviceSheet.Kind
        var id: String { kind == .ereader ? "ereader" : "token" }
    }

    func body(content: Content) -> some View {
        @Bindable var readers = readers
        content
            .sheet(item: Binding(get: { readers.adding.map(AddingKind.init) }, set: { if $0 == nil { readers.adding = nil } })) { k in
                NewReaderDeviceSheet(kind: k.kind, busy: readers.busy) { label, stream in onCreate(k.kind, label, stream) }
            }
            .sheet(item: $readers.secret) { $0.sheet }
            .confirmationDialog("Revoke \u{201C}\(readers.revoking?.label ?? "")\u{201D}?",
                                isPresented: Binding(get: { readers.revoking != nil }, set: { if !$0 { readers.revoking = nil } }),
                                titleVisibility: .visible, presenting: readers.revoking) { t in
                Button("Revoke", role: .destructive) { onRevoke(t) }
            } message: { _ in
                Text("Anything using it can no longer add books.")
            }
            .alert("Couldn't do that", isPresented: Binding(get: { readers.failure != nil }, set: { if !$0 { readers.failure = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(readers.failure ?? "")
            }
    }
}

/// Settings › Account: the server, signing out, and how sync is doing.
struct AccountSettingsScreen: View {
    let info: SettingsInfo
    var now: Date = .now
    var onSave: (String) -> Void = { _ in }
    var onSignOut: () -> Void = {}
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
            .listRowBackground(Color.lapis.card)

            Section("Account") {
                LabeledContent("Status", value: accountText)
                if info.account == .signedIn {
                    Button("Sign out", role: .destructive, action: onSignOut)
                        .accessibilityIdentifier("settings.signOut")
                }
            }
            .listRowBackground(Color.lapis.card)

            Section {
                LabeledContent("Indicator") { SyncDotLabel(badge: SyncBadge.make(status: info.status, pending: info.pending)) }
                LabeledContent("Connection", value: SyncBadge.describe(info.status))
                LabeledContent("Last synced", value: info.lastSynced.map { RelativeTime.string($0, now: now) } ?? "Not yet")
                LabeledContent("Cursor") { Text("seq \(info.cursor)").monospacedDigit() }
                LabeledContent("Pending writes") { Text("\(info.pending)").monospacedDigit() }
                if let error = info.syncError {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Sync error").foregroundStyle(Color.lapis.red)
                        Text(error).font(.footnote).foregroundStyle(Color.lapis.ink3)
                    }
                }
                ForEach(info.failedDocs, id: \.0) { title, why in
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Couldn't fetch \u{201C}\(title)\u{201D}").foregroundStyle(Color.lapis.yellow)
                        Text(why).font(.footnote).foregroundStyle(Color.lapis.ink3).lineLimit(3)
                    }
                }
            } header: {
                Text("Sync")
            } footer: {
                Text("Edits made offline wait in the outbox and send in order when the connection is back.")
            }
            .listRowBackground(Color.lapis.card)

            if let error = info.lastError {
                Section("Last error") { Text(error).font(.footnote).foregroundStyle(Color.lapis.ink3) }
                    .listRowBackground(Color.lapis.card)
            }
        }
        .scrollContentBackground(.hidden)
        .groundBackground()
        .navigationTitle("Account")
        .navigationBarTitleDisplayMode(.inline)
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
            lastSynced: PreviewData.ago(40), cursor: 1842, pending: 0, alerts: .notDetermined, sharedLinks: 2
        ), profileName: "Tom Meaney", now: PreviewData.now)
    }
    .preferredColorScheme(.light)
}

#Preview("Settings, offline, dark") {
    NavigationStack {
        SettingsContent(info: SettingsInfo(
            serverURL: "http://127.0.0.1:7425", account: .notRequired, status: .waiting(retryIn: .seconds(8)),
            lastSynced: PreviewData.ago(3 * 3600), cursor: 977, pending: 3, lastError: "Could not connect to the server.",
            syncError: "The request timed out.", failedDocs: [("Roadmap", "http(status: 500)")]
        ), now: PreviewData.now)
    }
    .preferredColorScheme(.dark)
}
#endif
