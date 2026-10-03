#if targetEnvironment(macCatalyst)
import SwiftUI
import TaisceKit

/// A ```` ```sql db=<name> ```` block on the Mac: ▶ Run against one of
/// this Mac's data sources (Settings › Data sources), the result as a
/// table beneath. No `db=`: Run offers the sources and, after a run, to
/// save the choice into the fence.
struct SQLCodeCard: View {
    @Environment(AppModel.self) private var model
    let language: String
    let fence: SQLFence
    let code: String
    let attributes: [String: String]
    let context: CodeRunContext

    @FocusState private var practiceFocused: Bool
    /// the data-source editor, opened from "Add…" (prefilled)
    @State private var adding: DataSource?

    var store: SQLRunStore { model.sqlRuns }
    var edits: CodeRunStore { model.codeRuns }

    var body: some View {
        let s = store.state(context)
        let e = edits.state(context)
        let choice = SQLSourceChoice.resolve(fence: fence, db: attributes["db"], sources: model.dataSources.sources)
        VStack(alignment: .leading, spacing: 0) {
            header(s, e, choice)
            codeArea(e)
            sourceNote(choice)
            if let err = e.saveError ?? s.saveError {
                Text(err).font(.caption).foregroundStyle(Theme.rose).padding(.horizontal, 14).padding(.bottom, 8)
            }
            if s.hasOutput {
                Rectangle().fill(Theme.hairline).frame(height: 1)
                SQLOutputView(state: s, saveDB: saveDBButton(s))
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .card(Theme.surface2)
        .sheet(item: SQLTrustFlow.presentation(store, s)) { p in
            SQLTrustSheet(prompt: p, store: store, state: s)
        }
        .sheet(item: $adding) { draft in
            NavigationStack { DataSourceEditor(original: draft, isNew: true) }
        }
        .onChange(of: code) { _, new in e.followDoc(new) }
        // who wrote it: the same cached ledger as the go/shell card
        .task(id: code) { await edits.loadAuthors(context) }
        .accessibilityElement(children: .contain)
    }

    // MARK: pieces

    private func header(_ s: SQLBlockState, _ e: BlockRunState, _ choice: SQLSourceChoice) -> some View {
        HStack(spacing: 10) {
            Text(language)
                .docFont(.caption, weight: .medium)
                .foregroundStyle(Theme.secondary)
            if let db = attributes["db"], !db.isEmpty {
                Text("on \(db)")
                    .docFont(.caption2)
                    .foregroundStyle(Theme.secondary.opacity(0.8))
                    .lineLimit(1)
            }
            Spacer(minLength: 8)
            if e.practice == nil, let who = edits.writtenBy(context) {
                Text("written by \(who)")
                    .docFont(.caption2)
                    .foregroundStyle(Theme.secondary.opacity(0.8))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help("The last change to this query was by \(who)")
                    .accessibilityIdentifier("sql.writtenBy")
            }
            if e.practice != nil {
                Button("Revert") { edits.revert(e) }
                    .disabled(e.isSaving)
                if context.canSave {
                    Button(e.isSaving ? "Saving\u{2026}" : "Save to doc") { Task { await edits.save(e, context: context) } }
                        .disabled(e.isSaving || !e.isPracticeEdited)
                        .help("Propose this change to the doc, like an edit")
                }
            } else {
                Button("Edit to try") {
                    edits.beginPractice(e, docCode: code)
                    practiceFocused = true
                }
                .help("Change the query here to try it; the doc stays as it is")
            }
            runControl(s, e, choice)
        }
        .buttonStyle(.borderless)
        .font(.caption)
        .padding(.horizontal, 14)
        .padding(.top, 10)
        .padding(.bottom, 2)
    }

    @ViewBuilder private func runControl(_ s: SQLBlockState, _ e: BlockRunState, _ choice: SQLSourceChoice) -> some View {
        if s.isChecking {
            ProgressView().controlSize(.small)
                .accessibilityLabel("Checking who wrote this")
        } else if s.isRunning {
            Button(role: .destructive) { store.stop(s) } label: {
                Label("Stop", systemImage: "stop.fill")
            }
            .tint(Theme.rose)
            .accessibilityIdentifier("sql.stop")
        } else {
            switch choice {
            case .ready(let source):
                Button { run(s, e, source, picked: false) } label: {
                    Label("Run", systemImage: "play.fill")
                }
                .tint(Theme.accentActive)
                .keyboardShortcut(practiceFocused ? KeyboardShortcut(.return, modifiers: .command) : nil)
                .help("Run on \(source.name) (\(source.kind.label)\(source.allowWrites ? ", writes allowed" : ", read-only"))")
                .accessibilityIdentifier("sql.run")
            case .pick(let sources):
                Menu {
                    ForEach(sources) { src in
                        Button("\(src.name) (\(src.kind.label))") { run(s, e, src, picked: true) }
                    }
                    if !sources.isEmpty { Divider() }
                    Button("Add a data source\u{2026}") { adding = draft(name: "") }
                } label: {
                    Label("Run on\u{2026}", systemImage: "play.fill")
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
                .tint(Theme.accentActive)
                .help(sources.isEmpty ? "No data sources on this Mac yet" : "Choose a data source to run this on")
                .accessibilityIdentifier("sql.pick")
            case .unknown, .wrongKind:
                Label("Run", systemImage: "play.fill")
                    .foregroundStyle(Theme.secondary.opacity(0.6))
                    .accessibilityIdentifier("sql.run.disabled")
            }
        }
    }

    @ViewBuilder private func sourceNote(_ choice: SQLSourceChoice) -> some View {
        switch choice {
        case .unknown(let name):
            HStack(spacing: 6) {
                Text("No data source named \(name) on this Mac \u{2014}")
                    .foregroundStyle(Theme.amber)
                Button("Add\u{2026}") { adding = draft(name: name) }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier("sql.add")
            }
            .font(.caption)
            .padding(.horizontal, 14)
            .padding(.bottom, 10)
        case .wrongKind(let s, let wanted):
            Text("\(s.name) is a \(s.kind.label) source; this block is \(wanted.label).")
                .font(.caption)
                .foregroundStyle(Theme.amber)
                .padding(.horizontal, 14)
                .padding(.bottom, 10)
        default:
            EmptyView()
        }
    }

    @ViewBuilder private func codeArea(_ e: BlockRunState) -> some View {
        if e.practice != nil {
            TextEditor(text: Binding(get: { e.practice ?? code }, set: { e.practice = $0 }))
                .docFont(.footnote, design: .monospaced)
                .foregroundStyle(Theme.text)
                .autocorrectionDisabled()
                .textInputAutocapitalization(.never)
                .scrollContentBackground(.hidden)
                .focused($practiceFocused)
                .frame(minHeight: max(60, CGFloat((e.practice ?? code).split(separator: "\n", omittingEmptySubsequences: false).count) * 18 + 16))
                .padding(.horizontal, 9)
                .padding(.vertical, 6)
                .accessibilityIdentifier("sql.practice")
        } else {
            ScrollView(.horizontal, showsIndicators: false) {
                Text(code)
                    .docFont(.footnote, design: .monospaced)
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(14)
            }
            .accessibilityLabel("Query, \(language): \(code)")
        }
    }

    private func saveDBButton(_ s: SQLBlockState) -> AnyView? {
        guard s.pickedSource, context.canSave, attributes["db"]?.isEmpty ?? true, let name = s.sourceName, !s.isBusy else { return nil }
        return AnyView(
            Button(s.isSavingDB ? "Saving\u{2026}" : "Save db=\(name) to doc") { Task { await store.saveSource(s, context: context) } }
                .buttonStyle(.borderless)
                .font(.caption)
                .disabled(s.isSavingDB)
                .help("Name this source in the block's fence, so Run uses it next time")
                .accessibilityIdentifier("sql.saveDB")
        )
    }

    private func draft(name: String) -> DataSource {
        DataSource(name: name, kind: fence.requiredKind ?? .sqlite)
    }

    private func run(_ s: SQLBlockState, _ e: BlockRunState, _ source: DataSource, picked: Bool) {
        Task { await store.run(s, edit: e, context: context, docCode: code, source: source, picked: picked) }
    }
}

/// Status, then the last statement's rows as a table (or the error).
private struct SQLOutputView: View {
    let state: SQLBlockState
    let saveDB: AnyView?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 10) {
                status
                Spacer()
                if let saveDB { saveDB }
                if let set = state.outcome?.lastResultSet, !set.columns.isEmpty {
                    Menu("Copy") {
                        Button("Copy as TSV") { UIPasteboard.general.string = set.tsv }
                        Button("Copy as Markdown") { UIPasteboard.general.string = set.markdown }
                    }
                    .menuStyle(.borderlessButton)
                    .fixedSize()
                    .font(.caption)
                    .accessibilityIdentifier("sql.copy")
                }
            }
            if let o = state.outcome {
                if let f = o.failure {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(f.sql)
                            .docFont(.caption, design: .monospaced)
                            .foregroundStyle(Theme.secondary)
                            .lineLimit(3)
                        Text(f.error)
                            .docFont(.footnote, design: .monospaced)
                            .foregroundStyle(Theme.rose)
                            .textSelection(.enabled)
                    }
                    .accessibilityIdentifier("sql.error")
                } else if let set = o.lastResultSet, !set.columns.isEmpty {
                    SQLTable(set: set)
                    if set.isCapped {
                        Text("The first \(set.rows.count.formatted()) rows; there are more")
                            .docFont(.caption2)
                            .foregroundStyle(Theme.amber)
                    }
                }
            }
        }
        .padding(14)
    }

    @ViewBuilder private var status: some View {
        if state.isChecking || state.isRunning {
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                TimelineView(.periodic(from: .now, by: 0.1)) { ctx in
                    let phase = state.isChecking ? "Checking who wrote this\u{2026}" : "Running on \(state.sourceName ?? "")\u{2026}"
                    Text("\(phase) \(RunStatus.seconds(ctx.date.timeIntervalSince(state.startedAt ?? ctx.date)))")
                        .monospacedDigit()
                }
            }
            .docFont(.caption)
            .foregroundStyle(Theme.secondary)
        } else if let o = state.outcome {
            let line = SQLStatus.line(o)
            Text(line.text)
                .docFont(.caption, weight: .medium)
                .foregroundStyle(line.ok ? Theme.green : Theme.rose)
                .monospacedDigit()
                .accessibilityIdentifier("sql.status")
        }
    }
}

/// The rows: a header with types, monospaced cells, NULL dimmed, both
/// directions scrolling; rows lay out lazily (at most 1000).
struct SQLTable: View {
    let set: SQLResultSet
    static let charWidth: CGFloat = 7.8

    var widths: [CGFloat] { Self.columnWidths(set) }

    /// Each column as wide as its longest header, type or (sampled) cell,
    /// within 6-48 characters.
    static func columnWidths(_ set: SQLResultSet) -> [CGFloat] {
        set.columns.indices.map { i in
            var chars = max(set.columns[i].name.count, set.columns[i].type?.count ?? 0)
            for row in set.rows.prefix(200) where i < row.count {
                let cell = row[i] ?? "NULL"
                chars = max(chars, cell.prefix { $0 != "\n" }.count)
                if chars >= 48 { break }
            }
            return CGFloat(min(48, max(6, chars))) * charWidth + 16
        }
    }

    var body: some View {
        let w = widths
        ScrollView([.horizontal, .vertical]) {
            LazyVStack(alignment: .leading, spacing: 0, pinnedViews: [.sectionHeaders]) {
                Section {
                    ForEach(set.rows.indices, id: \.self) { r in
                        HStack(spacing: 0) {
                            ForEach(set.columns.indices, id: \.self) { c in
                                let v = c < set.rows[r].count ? set.rows[r][c] : nil
                                Text(v.map { $0.replacingOccurrences(of: "\n", with: "\u{21B5}") } ?? "NULL")
                                    .docFont(.footnote, design: .monospaced)
                                    .foregroundStyle(v == nil ? Theme.secondary.opacity(0.55) : Theme.text)
                                    .italic(v == nil)
                                    .lineLimit(1)
                                    .truncationMode(.tail)
                                    .padding(.horizontal, 8)
                                    .padding(.vertical, 3)
                                    .frame(width: w[c], alignment: .leading)
                            }
                        }
                        .background(r % 2 == 1 ? Theme.surface.opacity(0.45) : Color.clear)
                        .textSelection(.enabled)
                    }
                } header: {
                    HStack(spacing: 0) {
                        ForEach(set.columns.indices, id: \.self) { c in
                            VStack(alignment: .leading, spacing: 1) {
                                Text(set.columns[c].name)
                                    .docFont(.caption, weight: .semibold)
                                    .foregroundStyle(Theme.text)
                                if let t = set.columns[c].type, !t.isEmpty {
                                    Text(t)
                                        .docFont(.caption2, design: .monospaced)
                                        .foregroundStyle(Theme.secondary)
                                }
                            }
                            .lineLimit(1)
                            .padding(.horizontal, 8)
                            .padding(.vertical, 4)
                            .frame(width: w[c], alignment: .leading)
                        }
                    }
                    .background(Theme.surface2)
                    .overlay(alignment: .bottom) { Rectangle().fill(Theme.hairline).frame(height: 1) }
                }
            }
        }
        .frame(maxHeight: 360)
        .fixedSize(horizontal: false, vertical: true)
        .background(Theme.surface.opacity(0.4), in: .rect(cornerRadius: 8, style: .continuous))
        .accessibilityLabel("Result: \(set.columns.count) columns, \(SQLStatus.rows(set.rows.count))")
        .accessibilityIdentifier("sql.table")
    }
}

struct SQLTrustSheet: View {
    let prompt: SQLPrompt
    let store: SQLRunStore
    let state: SQLBlockState
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Run this query?")
                .font(.headline)
            if let who = prompt.lastEditedBy {
                Text("Last edited by \(who). Run it on this Mac?")
                    .font(.subheadline)
                    .foregroundStyle(Theme.secondary)
                    .accessibilityIdentifier("sql.trust.question")
            }
            if prompt.writes {
                Label("This source allows writes: \(prompt.source.name) can change data.", systemImage: "exclamationmark.triangle.fill")
                    .font(.subheadline)
                    .foregroundStyle(Theme.amber)
                    .accessibilityIdentifier("sql.trust.writes")
            }
            ScrollView([.vertical, .horizontal]) {
                Text(prompt.code)
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .fixedSize()
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(minHeight: 100, maxHeight: 360)
            .background(Theme.surface2, in: .rect(cornerRadius: 10, style: .continuous))
            Text("It runs on \(prompt.source.name) (\(prompt.source.kind.label), \(prompt.source.summary)) with the credentials saved on this Mac.")
                .font(.caption)
                .foregroundStyle(Theme.secondary)
            HStack {
                Spacer()
                Button("Cancel", role: .cancel) {
                    store.cancelPrompt(state)
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
                .accessibilityIdentifier("sql.trust.cancel")
                Button("Run") { SQLTrustFlow.runPressed(store, state, prompt) { dismiss() } }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .tint(prompt.writes ? Theme.amber : Theme.accent)
                    .accessibilityIdentifier("sql.trust.run")
            }
        }
        .padding(20)
        .frame(minWidth: 460, idealWidth: 560)
    }
}
#endif
