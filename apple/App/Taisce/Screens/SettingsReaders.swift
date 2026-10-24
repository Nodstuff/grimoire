import SwiftUI
import TaisceKit
import UIKit

/// Settings › E-readers and Upload Tokens (SERVER mode, the person's own
/// app; the server refuses connectors and PATs). Secrets come back exactly
/// once, are shown with copy buttons, and are never stored on this device.
@MainActor @Observable
final class ReaderDevicesStore {
    enum Phase: Equatable {
        case loading
        /// LOCAL mode, or a server without books: the sections hide
        case unavailable
        case ready
        case failed(String)
    }

    private(set) var phase: Phase = .loading
    private(set) var ereaders: [DevicePasswordReader] = []
    private(set) var tokens: [UploadTokenInfo] = []
    /// a request in flight (buttons wait)
    private(set) var busy = false

    // what Settings presents (ReaderPresenters)
    var adding: NewReaderDeviceSheet.Kind?
    var secret: SecretPresentation?
    var revoking: UploadTokenInfo?
    var failure: String?

    struct SecretPresentation: Identifiable {
        let id = UUID()
        let sheet: SecretSheet
    }

    // MARK: the flows Settings runs

    /// Add an e-reader or create a token, then show its secret once.
    func create(_ kind: NewReaderDeviceSheet.Kind, label: String, stream: Bool, api: APIClient?, server: String, username: String?) async {
        switch kind {
        case .ereader:
            switch await pair(api: api, label: label, stream: stream) {
            case .success(let (e, password)):
                adding = nil
                secret = SecretPresentation(sheet: .device(e.label, setup: DevicePasswordSetup(server: server, username: username, password: password)))
            case .failure(let err): failure = err.message
            }
        case .token:
            switch await createToken(api: api, label: label) {
            case .success(let (t, token)):
                adding = nil
                secret = SecretPresentation(sheet: .token(t.label, secret: token, server: server))
            case .failure(let err): failure = err.message
            }
        }
    }

    func rotateAndShow(_ e: DevicePasswordReader, api: APIClient?, server: String, username: String?) async {
        switch await rotate(api: api, e) {
        case .success(let password):
            secret = SecretPresentation(sheet: .device(e.label, setup: DevicePasswordSetup(server: server, username: username, password: password)))
        case .failure(let err): failure = err.message
        }
    }

    func removeReporting(_ e: DevicePasswordReader, api: APIClient?) async {
        if case .failure(let err) = await remove(api: api, e) { failure = err.message }
    }

    func revokeReporting(_ t: UploadTokenInfo, api: APIClient?) async {
        if case .failure(let err) = await revoke(api: api, t) { failure = err.message }
    }

    func load(api: APIClient?) async {
        guard let api else { phase = .unavailable; return }
        do {
            async let e = api.ereaders()
            async let t = api.uploadTokens()
            let (ereaders, tokens) = try await (e, t)
            self.ereaders = ereaders.filter(\.isActive)
            self.tokens = tokens.filter(\.isActive)
            phase = .ready
        } catch APIError.notFound {
            phase = .unavailable
        } catch is CancellationError {
        } catch {
            if phase != .ready { phase = .failed(ReviewCopy.failure(error)) }
        }
    }

    /// The new device and its password, or why not.
    func pair(api: APIClient?, label: String, stream: Bool) async -> Result<(DevicePasswordReader, String), ReaderDeviceError> {
        await run(api) { api in
            let (e, password) = try await api.pairEreader(label: label, streamDownloads: stream)
            self.ereaders.append(e)
            return (e, password)
        }
    }

    func rotate(api: APIClient?, _ e: DevicePasswordReader) async -> Result<String, ReaderDeviceError> {
        await run(api) { api in try await api.rotateEreader(e.id) }
    }

    func remove(api: APIClient?, _ e: DevicePasswordReader) async -> Result<Void, ReaderDeviceError> {
        await run(api) { api in
            try await api.removeEreader(e.id)
            self.ereaders.removeAll { $0.id == e.id }
        }
    }

    func createToken(api: APIClient?, label: String) async -> Result<(UploadTokenInfo, String), ReaderDeviceError> {
        await run(api) { api in
            let (t, secret) = try await api.createUploadToken(label: label)
            self.tokens.append(t)
            return (t, secret)
        }
    }

    func revoke(api: APIClient?, _ t: UploadTokenInfo) async -> Result<Void, ReaderDeviceError> {
        await run(api) { api in
            try await api.revokeUploadToken(t.id)
            self.tokens.removeAll { $0.id == t.id }
        }
    }

    private func run<T>(_ api: APIClient?, _ body: (APIClient) async throws -> T) async -> Result<T, ReaderDeviceError> {
        guard let api else { return .failure(ReaderDeviceError(message: "Not connected.")) }
        busy = true
        defer { busy = false }
        do {
            return .success(try await body(api))
        } catch {
            return .failure(ReaderDeviceError(message: ReviewCopy.failure(error)))
        }
    }
}

struct ReaderDeviceError: Error, Equatable {
    var message: String
}

/// How a device or token's last use reads: "Last used 2 h ago", "Never used".
enum LastUsed {
    static func text(_ rfc3339: String?, now: Date = .now) -> String {
        guard let s = rfc3339, let d = ContinueReading.date(s) else { return "Never used" }
        return "Last used \(RelativeTime.string(d, now: now))"
    }
}

/// A secret shown once: each value with a copy button, and the warning.
struct SecretSheet: View {
    struct Row: Identifiable {
        var name: String
        var value: String
        var hint: String?
        var id: String { name }
    }

    let title: String
    let warning: String
    let rows: [Row]
    @Environment(\.dismiss) private var dismiss
    @State private var copied: String?

    var body: some View {
        NavigationStack {
            List {
                Section {
                    Label(warning, systemImage: "exclamationmark.triangle.fill")
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(Color.lapis.yellow)
                        .listRowBackground(Color.lapis.yellowSoft)
                }
                Section {
                    ForEach(rows) { row in
                        VStack(alignment: .leading, spacing: 4) {
                            Text(row.name).font(.footnote.weight(.semibold)).foregroundStyle(Color.lapis.ink3)
                            HStack(alignment: .center, spacing: 8) {
                                Text(row.value)
                                    .font(.lapisMono(.callout))
                                    .foregroundStyle(Color.lapis.ink)
                                    .textSelection(.enabled)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                Button {
                                    UIPasteboard.general.string = row.value
                                    copied = row.name
                                    Haptics.success()
                                } label: {
                                    Image(systemName: copied == row.name ? "checkmark" : "doc.on.doc")
                                        .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                                        .contentShape(.rect)
                                }
                                .buttonStyle(.borderless)
                                .accessibilityLabel(copied == row.name ? "\(row.name) copied" : "Copy \(row.name)")
                            }
                            if let hint = row.hint {
                                Text(hint).font(.caption).foregroundStyle(Color.lapis.ink3)
                            }
                        }
                        .padding(.vertical, 2)
                    }
                }
                .listRowBackground(Color.lapis.card)
            }
            .scrollContentBackground(.hidden)
            .groundBackground()
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
        .interactiveDismissDisabled()
    }

    /// Everything a device needs after pairing (or a new password).
    static func device(_ label: String, setup: DevicePasswordSetup) -> SecretSheet {
        var rows: [Row] = []
        if let u = setup.username { rows.append(Row(name: "Username", value: u, hint: "your Taisce name")) }
        rows += [
            Row(name: "Password", value: setup.password, hint: "if it is lost, New password makes another"),
            Row(name: "Catalog (OPDS)", value: setup.opds, hint: "with the username and password above"),
            Row(name: "Catalog, password in URL", value: setup.opdsPath, hint: "for readers without a login field"),
            Row(name: "Progress sync server", value: setup.progressSync, hint: "KOReader: Progress sync, custom server"),
        ]
        return SecretSheet(title: "Set up \(label)", warning: "You won't see this password again", rows: rows)
    }

    static func token(_ label: String, secret: String, server: String) -> SecretSheet {
        var base = server
        while base.hasSuffix("/") { base.removeLast() }
        return SecretSheet(title: label, warning: "You won't see this token again", rows: [
            Row(name: "Token", value: secret, hint: "it can only add books"),
            Row(name: "Upload URL", value: "\(base)/api/books", hint: "POST an EPUB as multipart field \u{201C}files\u{201D}, header Authorization: Bearer <token>"),
        ])
    }
}

/// A name, and for an e-reader the streaming switch; then Create.
struct NewReaderDeviceSheet: View {
    enum Kind { case ereader, token }

    let kind: Kind
    var busy = false
    let onCreate: (String, Bool) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var label = ""
    @State private var stream = false
    @FocusState private var focused: Bool

    private var trimmed: String { label.trimmingCharacters(in: .whitespacesAndNewlines) }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField(kind == .ereader ? "Device name, e.g. Kobo Libra" : "Name, e.g. iPhone Shortcut", text: $label)
                        .focused($focused)
                        .submitLabel(.done)
                        .onSubmit(create)
                        .onChange(of: label) { _, v in if v.count > readerDeviceLabelMax { label = String(v.prefix(readerDeviceLabelMax)) } }
                        .accessibilityIdentifier("readers.name")
                } footer: {
                    Text(kind == .ereader
                         ? "The device reads your books over a catalog feed (OPDS) and syncs its reading progress back."
                         : "A token can only add books: for a Shortcut, Calibre or a script.")
                }
                .listRowBackground(Color.lapis.card)
                if kind == .ereader {
                    Section {
                        Toggle("Stream downloads", isOn: $stream)
                    } footer: {
                        Text("Send book files through Taisce instead of redirecting the device to storage, for readers that can't follow the redirect.")
                    }
                    .listRowBackground(Color.lapis.card)
                }
            }
            .scrollContentBackground(.hidden)
            .groundBackground()
            .navigationTitle(kind == .ereader ? "Add E-reader" : "Create Upload Token")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button(kind == .ereader ? "Add" : "Create", action: create).disabled(trimmed.isEmpty || busy)
                }
            }
            .onAppear { focused = true }
        }
        .presentationDetents([.medium, .large])
    }

    private func create() {
        guard !trimmed.isEmpty, !busy else { return }
        onCreate(trimmed, stream)
    }
}

/// An e-reader's page: what it is, a new password, remove.
struct EreaderDetailScreen: View {
    let ereader: DevicePasswordReader
    let onRotate: () -> Void
    let onRemove: () -> Void
    @State private var confirmRotate = false
    @State private var confirmRemove = false

    var body: some View {
        List {
            Section {
                LabeledContent("Name", value: ereader.label)
                LabeledContent("Last used", value: LastUsed.text(ereader.lastUsedAt).replacingOccurrences(of: "Last used ", with: ""))
                LabeledContent("Downloads", value: ereader.streamDownloads ? "Streamed through Taisce" : "Straight from storage")
                if let added = ContinueReading.date(ereader.createdAt) {
                    LabeledContent("Added", value: RelativeTime.string(added))
                }
            }
            .listRowBackground(Color.lapis.card)
            Section {
                Button("New password\u{2026}") { confirmRotate = true }
                Button("Remove\u{2026}", role: .destructive) { confirmRemove = true }
            } footer: {
                Text("A new password stops the old one working; set it on the device again.")
            }
            .listRowBackground(Color.lapis.card)
        }
        .scrollContentBackground(.hidden)
        .groundBackground()
        .navigationTitle(ereader.label)
        .navigationBarTitleDisplayMode(.inline)
        .confirmationDialog("Make a new password for \u{201C}\(ereader.label)\u{201D}?", isPresented: $confirmRotate, titleVisibility: .visible) {
            Button("New password", action: onRotate)
        } message: {
            Text("The device stops syncing until it has the new one.")
        }
        .confirmationDialog("Remove \u{201C}\(ereader.label)\u{201D}?", isPresented: $confirmRemove, titleVisibility: .visible) {
            Button("Remove", role: .destructive, action: onRemove)
        } message: {
            Text("It can no longer read your books or sync progress.")
        }
    }
}

/// A rounded-square tile for a settings row (Lapis tones, not system colours).
struct SettingsTile: View {
    let symbol: String
    var fill: Color = Color.lapis.accent

    var body: some View {
        Image(systemName: symbol)
            .font(.subheadline.weight(.semibold))
            .foregroundStyle(Color.lapis.onAccent)
            .frame(width: 30, height: 30)
            .background(fill, in: .rect(cornerRadius: Lapis.Radius.md, style: .continuous))
            .accessibilityHidden(true)
    }
}
