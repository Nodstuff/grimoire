import SwiftUI
import TaisceKit

extension EnvironmentValues {
    /// The doc and block a code block belongs to (the reading view sets it);
    /// nil elsewhere (previews, search), where nothing runs.
    @Entry var codeRunContext: CodeRunContext?
}

/// A code block in read mode. On the Mac, `go`, `bash`, `sh` and `zsh`
/// fences get ▶ Run (output beneath), "Edit to try" and, for Go
/// declarations, a try line. Everywhere else, and on the iPhone, it is the
/// plain code card.
struct RunnableCodeBlock: View {
    let language: String?
    let code: String
    var attributes: [String: String] = [:]
    @Environment(\.codeRunContext) private var context

    var body: some View {
        #if targetEnvironment(macCatalyst)
        if let runnable = RunnableLanguage(language), let context {
            RunnableCodeCard(language: language ?? "", runnable: runnable, code: code, attributes: attributes, context: context)
        } else {
            CodeBlockView(label: language, code: code)
        }
        #else
        CodeBlockView(label: language, code: code)
        #endif
    }
}

#if targetEnvironment(macCatalyst)
private struct RunnableCodeCard: View {
    @Environment(AppModel.self) private var model
    let language: String
    let runnable: RunnableLanguage
    let code: String
    let attributes: [String: String]
    let context: CodeRunContext

    @FocusState private var focus: Field?
    enum Field: Hashable { case tryLine, practice }

    var store: CodeRunStore { model.codeRuns }

    var body: some View {
        let s = store.state(context)
        // read here so the card follows it (a sheet's binding getter is
        // called outside body, where Observation doesn't track)
        let prompt = s.trustPrompt
        VStack(alignment: .leading, spacing: 0) {
            header(s)
            codeArea(s)
            if needsTryLine(s) { tryLine(s) }
            if let err = s.saveError {
                Text(err).font(.caption).foregroundStyle(Theme.rose).padding(.horizontal, 14).padding(.bottom, 8)
            }
            if s.hasOutput {
                Rectangle().fill(Theme.hairline).frame(height: 1)
                RunOutputView(state: s, compiledOnly: compiledOnly(s))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .card(Theme.surface2)
        .sheet(item: TrustFlow.presentation(store, s)) { p in
            TrustSheet(prompt: p, store: store, state: s)
        }
        .accessibilityValue(prompt == nil ? "" : "asking before it runs")
        // an untouched practice text follows the doc's code
        .onChange(of: code) { _, new in s.followDoc(new) }
        // who wrote it: one ledger fetch per doc and epoch, cached in the store
        .task(id: code) { await store.loadAuthors(context) }
        .accessibilityElement(children: .contain)
    }

    // MARK: pieces

    private func header(_ s: BlockRunState) -> some View {
        HStack(spacing: 10) {
            Text(language)
                .docFont(.caption, weight: .medium)
                .foregroundStyle(Theme.secondary)
            if let cwd = attributes["cwd"], !cwd.isEmpty {
                Text("in \(cwd)")
                    .docFont(.caption2)
                    .foregroundStyle(Theme.secondary.opacity(0.8))
                    .lineLimit(1)
                    .truncationMode(.middle)
            }
            Spacer(minLength: 8)
            if s.practice == nil, let who = store.writtenBy(context) {
                Text("written by \(who)")
                    .docFont(.caption2)
                    .foregroundStyle(Theme.secondary.opacity(0.8))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help("The last change to this code was by \(who)")
                    .accessibilityIdentifier("code.writtenBy")
            }
            if s.practice != nil {
                Button("Revert") { store.revert(s) }
                    .disabled(s.isSaving)
                if context.canSave {
                    Button(s.isSaving ? "Saving\u{2026}" : "Save to doc") { Task { await store.save(s, context: context) } }
                        .disabled(s.isSaving || !s.isPracticeEdited)
                        .help("Propose this change to the doc, like an edit")
                }
            } else {
                Button("Edit to try") {
                    store.beginPractice(s, docCode: code)
                    focus = .practice
                }
                .help("Change the code here to try it; the doc stays as it is")
            }
            runButton(s)
        }
        .buttonStyle(.borderless)
        .font(.caption)
        .padding(.horizontal, 14)
        .padding(.top, 10)
        .padding(.bottom, 2)
    }

    @ViewBuilder private func runButton(_ s: BlockRunState) -> some View {
        if s.isChecking {
            ProgressView().controlSize(.small)
                .accessibilityLabel("Checking who wrote this")
        } else if s.isRunning {
            Button(role: .destructive) { store.stop(s) } label: {
                Label("Stop", systemImage: "stop.fill")
            }
            .tint(Theme.rose)
            .accessibilityIdentifier("code.stop")
        } else {
            Button { run(s) } label: {
                Label("Run", systemImage: "play.fill")
            }
            .tint(Theme.accentActive)
            // ⌘↩ runs the block whose try line or practice text has focus
            .keyboardShortcut(focus != nil ? KeyboardShortcut(.return, modifiers: .command) : nil)
            .help("Run on this Mac (⌘↩ while editing it)")
            .accessibilityIdentifier("code.run")
        }
    }

    @ViewBuilder private func codeArea(_ s: BlockRunState) -> some View {
        if s.practice != nil {
            TextEditor(text: Binding(get: { s.practice ?? code }, set: { s.practice = $0 }))
                .docFont(.footnote, design: .monospaced)
                .foregroundStyle(Theme.text)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .scrollContentBackground(.hidden)
                .focused($focus, equals: .practice)
                .frame(minHeight: max(60, CGFloat((s.practice ?? code).split(separator: "\n", omittingEmptySubsequences: false).count) * 18 + 16))
                .padding(.horizontal, 9)
                .padding(.vertical, 6)
                .accessibilityIdentifier("code.practice")
        } else {
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .docFont(.footnote, design: .monospaced)
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(14)
            }
            .accessibilityLabel("Code, \(language): \(code)")
        }
    }

    private func tryLine(_ s: BlockRunState) -> some View {
        HStack(spacing: 6) {
            Text("try")
                .docFont(.caption2, weight: .medium)
                .foregroundStyle(Theme.secondary)
            TextField(tryPlaceholder(s), text: Binding(get: { s.tryLine }, set: { s.tryLine = $0 }))
                .docFont(.footnote, design: .monospaced)
                .textFieldStyle(.plain)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .focused($focus, equals: .tryLine)
                .onSubmit { run(s) }
                .accessibilityIdentifier("code.tryLine")
        }
        .padding(.horizontal, 10)
        .padding(.vertical, 7)
        .background(Theme.surface.opacity(0.6), in: .rect(cornerRadius: 8, style: .continuous))
        .padding(.horizontal, 14)
        .padding(.bottom, 12)
    }

    // MARK: behaviour

    private func needsTryLine(_ s: BlockRunState) -> Bool {
        runnable.isGo && GoProgram.classify(s.code(doc: code)) == .declarations
    }

    private func tryPlaceholder(_ s: BlockRunState) -> String {
        if let f = GoProgram.firstFunction(in: s.code(doc: code)) { return "\(f)(\u{2026})  an expression to print; empty just compiles" }
        return "an expression to print; empty just compiles"
    }

    private func compiledOnly(_ s: BlockRunState) -> Bool {
        guard let r = s.result else { return false }
        return r.goShape == .declarations && r.succeeded && s.log.isEmpty
    }

    private func run(_ s: BlockRunState) {
        let request = RunRequest(language: runnable, code: s.code(doc: code), cwd: attributes["cwd"], tryLine: runnable.isGo ? s.tryLine : "")
        Task { await store.run(s, request, context: context, docCode: code) }
    }
}

/// The output under a block: status, then stdout and stderr in arrival
/// order (stderr tinted), the tail only when it is long.
private struct RunOutputView: View {
    let state: BlockRunState
    let compiledOnly: Bool
    /// characters laid out at most; the rest is one Copy away
    static let visible = 64 * 1024

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                status
                Spacer()
                if !state.log.isEmpty {
                    Button("Copy output") { UIPasteboard.general.string = state.log.text }
                        .buttonStyle(.borderless)
                        .font(.caption)
                }
            }
            let tail = state.log.tail(Self.visible)
            if tail.dropped {
                Text("\u{2026} earlier output not shown")
                    .docFont(.caption2)
                    .foregroundStyle(Theme.secondary)
            }
            if !tail.chunks.isEmpty {
                ScrollView(.vertical) {
                    Text(Self.attributed(tail.chunks))
                        .docFont(.footnote, design: .monospaced)
                        .textSelection(.enabled)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
                .frame(maxHeight: 320)
                .defaultScrollAnchor(.bottom)
            }
            if state.log.truncated {
                Text(OutputLog.truncatedMarker)
                    .docFont(.caption2)
                    .foregroundStyle(Theme.amber)
            }
        }
        .padding(14)
        .accessibilityElement(children: .combine)
    }

    static func attributed(_ chunks: [OutputChunk]) -> AttributedString {
        var out = AttributedString()
        for c in chunks {
            var a = AttributedString(c.text)
            a.foregroundColor = c.stream == .stderr ? Theme.rose : Theme.text
            out += a
        }
        return out
    }

    @ViewBuilder private var status: some View {
        if state.isChecking || state.isRunning {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                TimelineView(.periodic(from: .now, by: 0.1)) { ctx in
                    Text("\(state.phase ?? "Running\u{2026}") \(RunStatus.seconds(ctx.date.timeIntervalSince(state.startedAt ?? ctx.date)))")
                        .monospacedDigit()
                }
            }
            .docFont(.caption)
            .foregroundStyle(Theme.secondary)
        } else if let r = state.result {
            let line = RunStatus.line(r, compiledOnly: compiledOnly)
            Text(line.text)
                .docFont(.caption, weight: .medium)
                .foregroundStyle(line.ok ? Theme.green : Theme.rose)
                .monospacedDigit()
                .accessibilityIdentifier("code.status")
        }
    }
}

struct TrustSheet: View {
    let prompt: TrustPrompt
    let store: CodeRunStore
    let state: BlockRunState
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Run this code?")
                .font(.headline)
            Text("Last edited by \(prompt.lastEditedBy). Run it on this Mac?")
                .font(.subheadline)
                .foregroundStyle(Theme.secondary)
                .accessibilityIdentifier("code.trust.question")
            ScrollView([.vertical, .horizontal]) {
                Text(prompt.code)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 120, maxHeight: 420)
            .background(Theme.surface2, in: .rect(cornerRadius: 10, style: .continuous))
            Text("It runs \(prompt.request.cwd.map { "in \($0) " } ?? "")as you, with your login environment, including any tokens or keys in it.")
                .font(.caption)
                .foregroundStyle(Theme.secondary)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) { TrustFlow.cancelPressed(store, state) { dismiss() } }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("code.trust.cancel")
                Button("Run") { TrustFlow.runPressed(store, state, prompt) { dismiss() } }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .tint(Theme.accent)
                    .accessibilityIdentifier("code.trust.run")
            }
        }
        .padding(20)
        .frame(minWidth: 460, idealWidth: 560)
    }
}
#endif

/// The line under a finished run. Pure, so it is tested.
enum RunStatus {
    static func seconds(_ t: TimeInterval) -> String {
        t < 10 ? String(format: "%.2f s", t) : t < 60 ? String(format: "%.1f s", t) : "\(Int(t) / 60) min \(Int(t) % 60) s"
    }

    static func seconds(_ d: Duration) -> String {
        let (s, atto) = d.components
        return seconds(Double(s) + Double(atto) / 1e18)
    }

    static func line(_ r: RunResult, compiledOnly: Bool) -> (text: String, ok: Bool) {
        let t = seconds(r.duration)
        if let m = r.message { return (m, false) }
        if r.stopped { return ("Stopped · \(t)", false) }
        if r.timedOut { return ("Timed out after \(seconds(r.duration)) (limit 5 min)", false) }
        if r.buildFailed { return ("Build failed · \(t)", false) }
        if compiledOnly { return ("compiled \u{2713} · \(t)", true) }
        if let code = r.exitCode { return ("exit \(code) · \(t)", code == 0) }
        if let sig = r.signal { return ("killed by signal \(sig) · \(t)", false) }
        return ("finished · \(t)", false)
    }
}
