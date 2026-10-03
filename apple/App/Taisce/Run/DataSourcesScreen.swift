import SwiftUI
import UniformTypeIdentifiers
import TaisceKit

/// Settings › Data sources (Mac): the databases SQL blocks can run on.
struct DataSourcesScreen: View {
    @Environment(AppModel.self) private var model
    @State private var editing: DataSource?
    @State private var isNew = false

    var body: some View {
        let store = model.dataSources
        Form {
            Section {
                if store.sources.isEmpty {
                    Text("None yet. Add one, then a ```sql db=<name>``` block runs on it.")
                        .foregroundStyle(Theme.secondary)
                }
                ForEach(store.sources) { s in
                    Button {
                        isNew = false
                        editing = s
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(s.name).font(.body.monospaced()).foregroundStyle(Theme.text)
                                Text("\(s.kind.label) · \(s.summary)")
                                    .font(.caption)
                                    .foregroundStyle(Theme.secondary)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            }
                            Spacer()
                            if s.allowWrites {
                                Text("writes allowed")
                                    .font(.caption2.weight(.medium))
                                    .foregroundStyle(Theme.amber)
                            }
                            Image(systemName: "chevron.right").font(.caption).foregroundStyle(Theme.secondary)
                        }
                        .contentShape(.rect)
                    }
                    .buttonStyle(.plain)
                    .accessibilityIdentifier("datasource.\(s.name)")
                }
                Button("Add a data source\u{2026}") {
                    isNew = true
                    editing = DataSource(name: "", kind: .sqlite)
                }
                .accessibilityIdentifier("datasource.add")
            } footer: {
                Text("Kept on this Mac only: never synced or sent to your server. Passwords are in the Keychain. Without Allow writes, the database itself refuses changes.")
            }
            .listRowBackground(Theme.surface)
            if let err = store.loadError {
                Section { Text(err).font(.footnote).foregroundStyle(Theme.rose) }
                    .listRowBackground(Theme.surface)
            }
        }
        .groundBackground()
        .navigationTitle("Data sources")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $editing) { s in
            NavigationStack { DataSourceEditor(original: s, isNew: isNew) }
        }
    }
}

/// Add or edit one source: its fields, the password, Allow writes, Test
/// connection, Delete.
struct DataSourceEditor: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    let original: DataSource
    let isNew: Bool

    @State private var draft: DataSource
    /// nil = leave the stored password as it is
    @State private var password = ""
    @State private var passwordTouched = false
    @State private var error: String?
    @State private var testResult: (ok: Bool, text: String)?
    @State private var testing = false
    @State private var choosingFile = false
    @State private var confirmDelete = false

    init(original: DataSource, isNew: Bool) {
        self.original = original
        self.isNew = isNew
        _draft = State(initialValue: original)
    }

    var store: DataSourceStore { model.dataSources }

    var body: some View {
        Form {
            Section {
                TextField("Name (used as db=name)", text: $draft.name)
                    .font(.body.monospaced())
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .accessibilityIdentifier("datasource.name")
                Picker("Kind", selection: $draft.kind) {
                    ForEach(DataSourceKind.allCases) { Text($0.label).tag($0) }
                }
                .pickerStyle(.segmented)
                .accessibilityIdentifier("datasource.kind")
            }
            .listRowBackground(Theme.surface)

            Section("Connection") {
                switch draft.kind {
                case .sqlite:
                    HStack {
                        TextField("Database file", text: $draft.path)
                            .font(.body.monospaced())
                            .textInputAutocapitalization(.never)
                            .autocorrectionDisabled()
                            .accessibilityIdentifier("datasource.path")
                        Button("Choose\u{2026}") { choosingFile = true }
                    }
                case .postgres:
                    field("Host", $draft.host)
                    TextField("Port", value: $draft.port, format: .number.grouping(.never))
                        .keyboardType(.numberPad)
                    field("Database", $draft.database)
                    field("User", $draft.user)
                    passwordField
                    Picker("TLS", selection: $draft.tlsMode) {
                        ForEach(PostgresTLSMode.allCases) { Text($0.label).tag($0) }
                    }
                case .clickhouse:
                    field("URL, e.g. https://host:8443", $draft.url)
                        .keyboardType(.URL)
                    field("Database", $draft.database)
                    field("User", $draft.user)
                    passwordField
                }
            }
            .listRowBackground(Theme.surface)

            Section {
                Toggle("Allow writes", isOn: $draft.allowWrites)
                    .accessibilityIdentifier("datasource.allowWrites")
            } footer: {
                Text(writesFooter)
            }
            .listRowBackground(Theme.surface)

            Section {
                Button(testing ? "Testing\u{2026}" : "Test connection") { test() }
                    .disabled(testing)
                    .accessibilityIdentifier("datasource.test")
                if let r = testResult {
                    Text(r.text)
                        .font(.footnote)
                        .foregroundStyle(r.ok ? Theme.green : Theme.rose)
                        .textSelection(.enabled)
                        .accessibilityIdentifier("datasource.testResult")
                }
            }
            .listRowBackground(Theme.surface)

            if let error {
                Section { Text(error).font(.footnote).foregroundStyle(Theme.rose) }
                    .listRowBackground(Theme.surface)
            }

            if !isNew {
                Section {
                    Button("Delete this data source", role: .destructive) { confirmDelete = true }
                }
                .listRowBackground(Theme.surface)
            }
        }
        .groundBackground()
        .navigationTitle(isNew ? "New data source" : original.name)
        .navigationBarTitleDisplayMode(.inline)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save") { save() }
                    .accessibilityIdentifier("datasource.save")
            }
        }
        .fileImporter(isPresented: $choosingFile, allowedContentTypes: [.item]) { result in
            if case .success(let url) = result { draft.path = url.path(percentEncoded: false) }
        }
        .confirmationDialog("Delete \(original.name)?", isPresented: $confirmDelete, titleVisibility: .visible) {
            Button("Delete", role: .destructive) {
                do {
                    try store.delete(original)
                    dismiss()
                } catch {
                    self.error = error.localizedDescription
                }
            }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Blocks that say db=\(original.name) won't run on this Mac until you add it again.")
        }
        .onChange(of: draft.kind) { old, new in
            if new == .clickhouse, draft.user.isEmpty { draft.user = "default" }
            if old == .clickhouse, new == .postgres, draft.user == "default" { draft.user = "" }
            testResult = nil
        }
        .frame(minWidth: 480, minHeight: 520)
    }

    private func field(_ title: String, _ text: Binding<String>) -> some View {
        TextField(title, text: text)
            .textInputAutocapitalization(.never)
            .autocorrectionDisabled()
    }

    private var passwordField: some View {
        SecureField(!isNew && store.hasPassword(original) && !passwordTouched ? "Password (saved; type to change)" : "Password", text: Binding(
            get: { password },
            set: {
                password = $0
                passwordTouched = true
            }
        ))
        .accessibilityIdentifier("datasource.password")
    }

    private var writesFooter: String {
        switch draft.kind {
        case .sqlite: draft.allowWrites ? "The file is opened read-write. Every run asks first." : "The file is opened read-only: SQLite refuses any change."
        case .postgres: draft.allowWrites ? "Writes are allowed. Every run asks first." : "Sessions start read-only (default_transaction_read_only). For a hard guarantee, connect as a role with only SELECT."
        case .clickhouse: draft.allowWrites ? "Writes are allowed. Every run asks first." : "Queries run with readonly=2: ClickHouse refuses changes."
        }
    }

    /// What Save would store: nil keeps the saved password.
    private var passwordToSave: String? {
        passwordTouched ? password : nil
    }

    private func test() {
        testing = true
        testResult = nil
        let candidate = draft
        // typed: that one; else the saved one (none for a new source)
        let pw = passwordToSave
        Task {
            testResult = await store.test(candidate, password: pw)
            testing = false
        }
    }

    private func save() {
        do {
            try store.save(draft, password: passwordToSave)
            dismiss()
        } catch {
            self.error = SQLRunner.describe(error)
        }
    }
}
