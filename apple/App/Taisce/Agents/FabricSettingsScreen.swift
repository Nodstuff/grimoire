import SwiftUI
import TaisceKit
#if canImport(VisionKit)
import VisionKit
#endif

/// Settings › Agents: this phone's key and which Macs it is paired with;
/// pairing a new one; the guardrails, limits and fabrics every runtime
/// enforces (one signed, versioned Settings); each project's Text off and
/// private notifications.
struct FabricSettingsScreen: View {
    @Environment(AppModel.self) private var model
    @State private var draft: FabricSettings?
    @State private var saving = false
    @State private var toast: ToastMessage?
    @State private var pairing = false
    @State private var editing: GuardrailRef?
    @State private var editingFabric: FabricRef?

    private var store: FleetStore { model.fleet }
    private var config: FabricConfigStore { store.config }

    var body: some View {
        let current = config.settings
        let working = draft ?? current
        Form {
            phoneSection
            Section {
                ForEach(Array(working.guardrails.enumerated()), id: \.element.id) { i, g in
                    Toggle(isOn: Binding(get: { working.guardrails[i].enabled }, set: { on in edit { $0.guardrails[i].enabled = on } })) {
                        Button { editing = GuardrailRef(index: i) } label: {
                            VStack(alignment: .leading, spacing: 2) {
                                Text(g.label).foregroundStyle(Color.lapis.ink)
                                Text(g.builtin.isEmpty ? g.pattern : "built in: \(g.builtin.replacingOccurrences(of: "_", with: " "))")
                                    .font(.system(.caption, design: .monospaced)).foregroundStyle(Color.lapis.ink3).lineLimit(1)
                            }
                        }
                        .buttonStyle(.plain)
                    }
                    .accessibilityIdentifier("guardrail.\(g.id)")
                }
                .onDelete { idx in edit { $0.guardrails.remove(atOffsets: idx) } }
                Button("Add a guardrail", systemImage: "plus") {
                    edit { $0.guardrails.append(FabricGuardrail(id: UUID().uuidString.lowercased(), label: "New guardrail", tools: ["Bash"], pattern: "")) }
                    editing = GuardrailRef(index: (draft ?? current).guardrails.count - 1)
                }
                .accessibilityIdentifier("guardrail.add")
            } header: {
                Text("Guardrails")
            } footer: {
                Text("A tool call any of these matches waits for your approval. A pattern your Mac can't run makes it refuse the whole set and keep the rules it has.")
            }
            limitsSection(working)
            Section {
                ForEach(Array(working.fabrics.enumerated()), id: \.element.id) { i, f in
                    Button { editingFabric = FabricRef(index: i) } label: {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(f.name).foregroundStyle(Color.lapis.ink)
                            let members = store.fleet.sessions.filter { $0.fabricID == f.id }.map(\.address)
                            Text(([f.purpose] + (members.isEmpty ? [] : [members.joined(separator: ", ")])).filter { !$0.isEmpty }.joined(separator: "\n"))
                                .font(.caption).foregroundStyle(Color.lapis.ink3).lineLimit(3)
                        }
                    }
                    .accessibilityIdentifier("fabric.\(f.name)")
                }
                .onDelete { idx in edit { $0.fabrics.remove(atOffsets: idx) } }
                Button("New fabric", systemImage: "plus") {
                    edit { $0.fabrics.append(FabricDefinition(id: UUID().uuidString.lowercased(), name: "")) }
                    editingFabric = FabricRef(index: (draft ?? current).fabrics.count - 1)
                }
                .accessibilityIdentifier("fabric.add")
            } header: {
                Text("Fabrics")
            } footer: {
                Text("A fabric is a named group of sessions working on one thing; each session starts with a brief of its purpose. Move a session into one from its screen. A session not moved is in its project's own.")
            }
            projectsSection
            if let view = config.view {
                Section("In force") {
                    LabeledContent("Saved version", value: view.defaults ? "built-in defaults" : "\(view.version)")
                    ForEach(store.fleet.runtimes) { r in
                        LabeledContent(r.name, value: r.settingsVersion == 0 ? "defaults" : "version \(r.settingsVersion)")
                    }
                }
            }
        }
        .scrollContentBackground(.hidden)
        .groundBackground()
        .navigationTitle("Agents")
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) {
            if let draft, draft != current {
                HStack(spacing: 10) {
                    Button("Discard") { self.draft = nil }.buttonStyle(.lapis(.secondary))
                    Button(saving ? "Signing\u{2026}" : "Sign and save") { save(draft) }
                        .buttonStyle(.lapis(.primary, fullWidth: true))
                        .disabled(saving)
                        .accessibilityIdentifier("settings.fabric.save")
                }
                .padding(.horizontal, Lapis.Layout.gutter)
                .padding(.vertical, 10)
                .background(.bar)
            }
        }
        .sheet(isPresented: $pairing) { PairingSheet() }
        .sheet(item: $editing) { ref in
            GuardrailEditor(guardrail: (draft ?? current).guardrails[safe: ref.index] ?? FabricGuardrail(id: "", label: "")) { g in
                edit { if $0.guardrails.indices.contains(ref.index) { $0.guardrails[ref.index] = g } }
            }
        }
        .sheet(item: $editingFabric) { ref in
            FabricEditor(fabric: (draft ?? current).fabrics[safe: ref.index] ?? FabricDefinition(id: "", name: "")) { f in
                edit { if $0.fabrics.indices.contains(ref.index) { $0.fabrics[ref.index] = f } }
            }
        }
        .lapisToast($toast)
        .task { await config.load() }
        .refreshable { await config.load() }
    }

    private func edit(_ change: (inout FabricSettings) -> Void) {
        var d = draft ?? config.settings
        change(&d)
        draft = d
    }

    private func save(_ next: FabricSettings) {
        saving = true
        Task {
            let failure = await config.save(next)
            saving = false
            if let failure {
                toast = ToastMessage(text: failure, tone: .error)
            } else {
                draft = nil
                toast = ToastMessage(text: "Signed and saved: your Macs take it as they connect", tone: .success)
            }
        }
    }

    // MARK: sections

    private var phoneSection: some View {
        let key = store.command.identity.existing()
        return Section {
            if let key {
                LabeledContent("This phone") {
                    Text(key.fingerprint).font(.system(.body, design: .monospaced)).foregroundStyle(Color.lapis.ink)
                }
                .accessibilityIdentifier("settings.fabric.fingerprint")
            }
            ForEach(store.fleet.runtimes) { r in
                let paired = key.map { r.isPaired($0.deviceID) } ?? false
                LabeledContent(r.name) {
                    Label(paired ? "Paired" : "Not paired", systemImage: paired ? "checkmark.seal.fill" : "seal")
                        .foregroundStyle(paired ? Color.lapis.green : Color.lapis.ink3)
                }
                .accessibilityIdentifier("settings.fabric.mac.\(r.name)")
            }
            Button("Pair with a Mac", systemImage: "qrcode.viewfinder") { pairing = true }
                .accessibilityIdentifier("settings.fabric.pair")
        } header: {
            Text("This phone")
        } footer: {
            Text(store.command.identity.kind == .software && key != nil
                 ? "This device has no Secure Enclave (a simulator): its key is kept in the Keychain instead."
                 : "Replies, approvals and settings from this phone are signed with a key that never leaves its Secure Enclave. A Mac obeys only devices paired with it there. Remove a device at the Mac.")
        }
    }

    private func limitsSection(_ s: FabricSettings) -> some View {
        let l = (s.limits ?? FabricLimits()).effective
        func stepper(_ title: String, _ value: Int, _ range: ClosedRange<Int>, _ step: Int = 1, _ set: @escaping (inout FabricLimits, Int) -> Void) -> some View {
            Stepper(value: Binding(get: { value }, set: { v in edit { var lim = ($0.limits ?? FabricLimits()).effective; set(&lim, v); $0.limits = lim } }), in: range, step: step) {
                LabeledContent(title, value: "\(value)")
            }
        }
        return Section {
            stepper("Messages between two sessions", l.pairMessages, 1...200) { $0.pairMessages = $1 }
            stepper("Agent-started turns an hour", l.agentTurnsPerHour, 1...200) { $0.agentTurnsPerHour = $1 }
            stepper("Wakes of one session an hour", l.wakesPerSessionPerHour, 1...60) { $0.wakesPerSessionPerHour = $1 }
            stepper("Don't wake at context %", l.coldContextPct, 10...100, 5) { $0.coldContextPct = $1 }
            stepper("\u{2026}idle for minutes", l.coldIdleMinutes, 5...600, 5) { $0.coldIdleMinutes = $1 }
        } header: {
            Text("Limits")
        } footer: {
            Text("Two sessions that reach the message limit pause until you say continue. An idle session with a nearly full context is never woken automatically: you're asked instead.")
        }
    }

    private var projectsSection: some View {
        Section {
            if config.projects.isEmpty {
                Text("Projects show here once a session reports one.").foregroundStyle(Color.lapis.ink3)
            }
            ForEach(config.projects) { p in
                VStack(alignment: .leading, spacing: 4) {
                    Text(p.name).font(.headline)
                    Toggle("Text off", isOn: Binding(get: { p.textOff }, set: { v in act { await config.setProject(p.name, textOff: v) } }))
                        .accessibilityIdentifier("project.\(p.name).textOff")
                    Toggle("Private notifications", isOn: Binding(get: { p.privateNotifications }, set: { v in act { await config.setProject(p.name, privateNotifications: v) } }))
                        .accessibilityIdentifier("project.\(p.name).private")
                }
            }
        } header: {
            Text("Projects")
        } footer: {
            Text("Text off: turns, questions and last lines are not stored on your server (what must reach a sleeping Mac is kept only until delivered), and turning it on deletes what is stored. Private notifications say only the state, never the words.")
        }
    }

    private func act(_ change: @escaping @MainActor () async -> String?) {
        Task { if let f = await change() { toast = ToastMessage(text: f, tone: .error) } }
    }
}

struct GuardrailRef: Identifiable { let index: Int; var id: Int { index } }
struct FabricRef: Identifiable { let index: Int; var id: Int { index } }

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

/// One guardrail: what it is called, which tools, the pattern (checked as
/// you type: a pattern that doesn't compile is never signed).
struct GuardrailEditor: View {
    @State var guardrail: FabricGuardrail
    let onDone: (FabricGuardrail) -> Void
    @Environment(\.dismiss) private var dismiss
    @State private var tools = ""
    @State private var sample = ""

    init(guardrail: FabricGuardrail, onDone: @escaping (FabricGuardrail) -> Void) {
        _guardrail = State(initialValue: guardrail)
        _tools = State(initialValue: guardrail.tools.joined(separator: ", "))
        self.onDone = onDone
    }

    private var regex: NSRegularExpression? { try? NSRegularExpression(pattern: guardrail.pattern) }

    var body: some View {
        NavigationStack {
            Form {
                Section("Name") {
                    TextField("Force push", text: $guardrail.label).accessibilityIdentifier("guardrail.label")
                    Toggle("On", isOn: $guardrail.enabled)
                }
                Section {
                    TextField("Bash", text: $tools).autocorrectionDisabled().textInputAutocapitalization(.never)
                } header: { Text("Tools") } footer: { Text("Comma-separated: Bash, Write, Edit\u{2026}; * for every tool. Bash is matched on its command, the file tools on their path.") }
                if guardrail.builtin.isEmpty {
                    Section {
                        TextField(#"\bgit\s+push\b.*--force"#, text: $guardrail.pattern, axis: .vertical)
                            .font(.system(.body, design: .monospaced))
                            .autocorrectionDisabled().textInputAutocapitalization(.never)
                            .accessibilityIdentifier("guardrail.pattern")
                        if !guardrail.pattern.isEmpty, regex == nil {
                            Label("This pattern doesn't compile", systemImage: "exclamationmark.triangle").foregroundStyle(Color.lapis.red)
                        }
                        TextField("Try it on a command", text: $sample)
                            .font(.system(.body, design: .monospaced))
                            .autocorrectionDisabled().textInputAutocapitalization(.never)
                        if !sample.isEmpty, let regex {
                            let hit = regex.firstMatch(in: sample, range: NSRange(sample.startIndex..., in: sample)) != nil
                            Label(hit ? "Would wait for you" : "Would run", systemImage: hit ? "hand.raised" : "checkmark")
                                .foregroundStyle(hit ? Color.lapis.yellow : Color.lapis.green)
                        }
                    } header: { Text("Pattern") } footer: { Text("Found anywhere in the command, case-sensitive unless it starts (?i). No lookaround or backreferences: your Mac uses the same syntax as Rust's regex.") }
                } else {
                    Section("Check") { Text("Built into the runtime: \(guardrail.builtin.replacingOccurrences(of: "_", with: " "))") }
                }
            }
            .navigationTitle(guardrail.label.isEmpty ? "Guardrail" : guardrail.label)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        var g = guardrail
                        g.tools = tools.split(separator: ",").map { $0.trimmingCharacters(in: .whitespaces) }.filter { !$0.isEmpty }
                        onDone(g)
                        dismiss()
                    }
                    .disabled(guardrail.label.isEmpty || (guardrail.builtin.isEmpty && (guardrail.pattern.isEmpty || regex == nil)))
                    .accessibilityIdentifier("guardrail.done")
                }
            }
        }
    }
}

/// A fabric: its name (agents address a note to every member by it), one
/// sentence of purpose, and the doc that explains the work.
struct FabricEditor: View {
    @State var fabric: FabricDefinition
    let onDone: (FabricDefinition) -> Void
    @Environment(\.dismiss) private var dismiss

    private var nameProblem: String? {
        let n = fabric.name.trimmingCharacters(in: .whitespaces)
        if n.contains("/") || n.contains(":") { return "No / or : in a name: it would read as an address." }
        return nil
    }

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("runtime-channel", text: $fabric.name)
                        .autocorrectionDisabled().textInputAutocapitalization(.never)
                        .accessibilityIdentifier("fabricEditor.name")
                    if let nameProblem { Text(nameProblem).foregroundStyle(Color.lapis.red) }
                } header: { Text("Name") }
                Section {
                    TextField("What these sessions are working on, in one sentence", text: $fabric.purpose, axis: .vertical)
                        .accessibilityIdentifier("fabricEditor.purpose")
                } header: { Text("Purpose") } footer: { Text("Every session in the fabric starts with this in its brief.") }
                Section("Doc") {
                    TextField("Title of the Taisce doc that explains the work", text: $fabric.docTitle)
                }
            }
            .navigationTitle(fabric.name.isEmpty ? "New fabric" : fabric.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        fabric.name = fabric.name.trimmingCharacters(in: .whitespaces)
                        onDone(fabric)
                        dismiss()
                    }
                    .disabled(fabric.name.trimmingCharacters(in: .whitespaces).isEmpty || nameProblem != nil)
                    .accessibilityIdentifier("fabricEditor.done")
                }
            }
        }
    }
}

/// Pair this phone with a Mac: read the code its runtime shows (scan it,
/// or paste it), seal this phone's key for it, then compare fingerprints
/// and confirm at the Mac.
struct PairingSheet: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var code = ""
    @State private var name = UIDevice.current.name
    @State private var pairingID: String?
    @State private var failure: String?
    @State private var scanning = false
    @State private var busy = false

    private var config: FabricConfigStore { model.fleet.config }

    var body: some View {
        NavigationStack {
            Form {
                if let pairingID {
                    waiting(pairingID)
                } else {
                    Section {
                        #if canImport(VisionKit) && !targetEnvironment(macCatalyst)
                        if DataScannerViewController.isSupported {
                            Button("Scan the code on your Mac", systemImage: "qrcode.viewfinder") { scanning = true }
                        }
                        #endif
                        TextField("taisce-pair:?v=1&\u{2026}", text: $code, axis: .vertical)
                            .font(.system(.footnote, design: .monospaced))
                            .autocorrectionDisabled().textInputAutocapitalization(.never)
                            .accessibilityIdentifier("pairing.code")
                    } header: { Text("The code") } footer: {
                        Text("In Workbox on your Mac: Settings \u{203A} Pair a phone. It shows a code for ten minutes.")
                    }
                    Section("This phone's name, as the Mac will list it") {
                        TextField("iPhone", text: $name).accessibilityIdentifier("pairing.name")
                    }
                    if let failure {
                        Section { Text(failure).foregroundStyle(Color.lapis.red).accessibilityIdentifier("pairing.failure") }
                    }
                    Section {
                        Button(busy ? "Sending\u{2026}" : "Pair") { pair() }
                            .disabled(code.isEmpty || name.isEmpty || busy)
                            .accessibilityIdentifier("pairing.submit")
                    }
                }
            }
            .navigationTitle("Pair with a Mac")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar { ToolbarItem(placement: .cancellationAction) { Button(pairingID == nil ? "Cancel" : "Done") { dismiss() } } }
            #if canImport(VisionKit) && !targetEnvironment(macCatalyst)
            .sheet(isPresented: $scanning) {
                CodeScanner { text in
                    code = text
                    scanning = false
                    pair()
                }
                .ignoresSafeArea()
            }
            #endif
        }
    }

    @ViewBuilder
    private func waiting(_ id: String) -> some View {
        let view = config.pairings[id]
        let mine = model.fleet.command.identity.existing()?.fingerprint ?? ""
        Section {
            VStack(alignment: .leading, spacing: 10) {
                Text("Check your Mac shows this phone as").font(.subheadline).foregroundStyle(Color.lapis.ink2)
                Text(mine).font(.system(.title2, design: .monospaced).weight(.semibold)).foregroundStyle(Color.lapis.ink)
                    .accessibilityIdentifier("pairing.fingerprint")
                Text("and confirm there only if it matches.").font(.subheadline).foregroundStyle(Color.lapis.ink2)
                if let c = config.code(for: id) {
                    Text("The Mac's own key: \(c.runtimeFingerprint)").font(.caption.monospaced()).foregroundStyle(Color.lapis.ink3)
                }
            }
            .padding(.vertical, 6)
        }
        Section {
            switch view?.state ?? .pending {
            case .paired:
                Label("Paired. This phone can now reply, approve and answer.", systemImage: "checkmark.seal.fill").foregroundStyle(Color.lapis.green)
                    .accessibilityIdentifier("pairing.paired")
            case .rejected:
                Label("The Mac said the fingerprint didn't match, or cancelled.", systemImage: "xmark.seal").foregroundStyle(Color.lapis.red)
            case .expired:
                Label("That code has expired. Show a new one on the Mac.", systemImage: "clock.badge.xmark").foregroundStyle(Color.lapis.red)
            case .invalid:
                Label(view?.detail ?? "The Mac couldn't open or check what this phone sent.", systemImage: "exclamationmark.shield").foregroundStyle(Color.lapis.red)
            default:
                HStack(spacing: 10) { ProgressView(); Text("Waiting for you to confirm at the Mac\u{2026}") }
                    .accessibilityIdentifier("pairing.waiting")
            }
        }
        .task { await config.watch(id) }
    }

    private func pair() {
        busy = true
        failure = nil
        Task {
            let host = URL(string: model.serverURL).flatMap { u in u.host().map { h in h + (u.port.map { ":\($0)" } ?? "") } }
            switch await config.pair(code: code, deviceName: name, serverHost: host) {
            case .success(let id): pairingID = id
            case .failure(.words(let w)): failure = w
            }
            busy = false
        }
    }
}

#if canImport(VisionKit) && !targetEnvironment(macCatalyst)
/// The camera, reading QR codes until one is a pairing code.
struct CodeScanner: UIViewControllerRepresentable {
    let onCode: (String) -> Void

    func makeUIViewController(context: Context) -> DataScannerViewController {
        let vc = DataScannerViewController(recognizedDataTypes: [.barcode(symbologies: [.qr])], qualityLevel: .balanced, isHighlightingEnabled: true)
        vc.delegate = context.coordinator
        try? vc.startScanning()
        return vc
    }

    func updateUIViewController(_ vc: DataScannerViewController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(onCode: onCode) }

    final class Coordinator: NSObject, DataScannerViewControllerDelegate {
        let onCode: (String) -> Void
        var done = false
        init(onCode: @escaping (String) -> Void) { self.onCode = onCode }

        func dataScanner(_ scanner: DataScannerViewController, didAdd items: [RecognizedItem], allItems: [RecognizedItem]) {
            guard !done else { return }
            for item in items {
                if case .barcode(let b) = item, let s = b.payloadStringValue, s.hasPrefix("taisce-pair:") {
                    done = true
                    scanner.stopScanning()
                    onCode(s)
                    return
                }
            }
        }
    }
}
#endif
