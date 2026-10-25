import SwiftUI
import TaisceKit

/// A library book in the in-app reader (the shell's `Route.bookReader(id)`,
/// Continue reading, a cover's Read).
struct BookReaderScreen: View {
    let bookID: String
    @Environment(AppModel.self) private var model

    var body: some View {
        ReaderScreen(source: .book(bookID), title: model.books.book(bookID)?.title ?? "")
    }
}

/// The reader: the page edge to edge on its paper, a bar that steps aside
/// while reading (Back, title and chapter, Contents, Settings), and the
/// progress line. Tap the middle for the bar, the edges to turn; swipe;
/// arrows, Space, J/K on a keyboard.
struct ReaderScreen: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.horizontalSizeClass) private var sizeClass
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.scenePhase) private var scenePhase
    @State private var reader: ReaderModel
    @State private var showContents = false
    @State private var showSettings = false

    init(source: ReadingSource, title: String) {
        _reader = State(initialValue: ReaderModel(source: source, title: title, store: nil))
        self.source = source
        self.initialTitle = title
    }

    private let source: ReadingSource
    private let initialTitle: String

    var body: some View {
        ReaderBody(reader: reader, showContents: $showContents, showSettings: $showSettings, close: close)
            .task {
                reader.attach(model.books)
                reader.environment(dark: colorScheme == .dark, phone: sizeClass == .compact, reduceMotion: reduceMotion)
                await reader.start()
            }
            .onChange(of: colorScheme) { reader.environment(dark: colorScheme == .dark, phone: sizeClass == .compact, reduceMotion: reduceMotion) }
            .onChange(of: sizeClass) { reader.environment(dark: colorScheme == .dark, phone: sizeClass == .compact, reduceMotion: reduceMotion) }
            .onChange(of: scenePhase) { _, phase in
                switch phase {
                case .active: Task { await reader.resumed() }
                case .background, .inactive: Task { await reader.backgrounded() }
                @unknown default: break
                }
            }
            .onDisappear { Task { await reader.close() } }
            .toolbar(.hidden, for: .navigationBar)
            .toolbar(.hidden, for: .tabBar)
            .navigationBarBackButtonHidden()
            .statusBarHidden(!reader.chromeVisible)
            .persistentSystemOverlays(reader.chromeVisible ? .automatic : .hidden)
            .lapisToast($reader.toast)
            .sheet(isPresented: $showContents) {
                ReaderContents(reader: reader) { showContents = false }
                    .presentationDetents([.medium, .large])
            }
            .sheet(isPresented: $showSettings) {
                ReaderSettingsSheet(reader: reader)
                    .presentationDetents([.height(460), .large])
            }
    }

    private func close() {
        Task {
            await reader.close()
            dismiss()
        }
    }
}

private struct ReaderBody: View {
    @Bindable var reader: ReaderModel
    @Binding var showContents: Bool
    @Binding var showSettings: Bool
    let close: () -> Void

    var paperColor: Color { Color(hexCSS: ReaderPaperColours.of(reader.paper).paper) }
    var inkColor: Color { Color(hexCSS: ReaderPaperColours.of(reader.paper).ink) }
    var scheme: ColorScheme { ReaderPaperColours.of(reader.paper).dark ? .dark : .light }

    var body: some View {
        ZStack {
            paperColor.ignoresSafeArea()
            ReaderWebView(page: reader.page)
                .ignoresSafeArea(edges: .horizontal)
                .padding(.top, 8)
                .opacity(reader.phase == .reading ? 1 : 0)
                .accessibilityLabel(reader.title)
                .accessibilityAction(named: "Next page") { reader.next() }
                .accessibilityAction(named: "Previous page") { reader.prev() }
                .accessibilityAction(named: "Contents") { showContents = true }
            switch reader.phase {
            case .loading:
                VStack(spacing: Lapis.Space.s3) {
                    ProgressView().tint(inkColor)
                    Text("Opening \(reader.title.isEmpty ? "the book" : reader.title)…")
                        .font(.subheadline)
                        .foregroundStyle(inkColor.opacity(0.75))
                }
                .accessibilityElement(children: .combine)
            case let .failed(message):
                EmptyState(symbol: "book.closed", title: "The book didn't open", message: message,
                           actionTitle: reader.canRetry ? "Try Again" : "Close", action: reader.canRetry ? reader.retry : close)
                    .padding()
                    .background(Color.lapis.bg, in: .rect(cornerRadius: Lapis.Radius.card))
                    .padding()
            case .reading:
                EmptyView()
            }
        }
        .overlay(alignment: .top) {
            if reader.chromeVisible || reader.phase != .reading {
                topBar.transition(.move(edge: .top).combined(with: .opacity))
            }
        }
        .overlay(alignment: .bottom) {
            if reader.phase == .reading {
                bottomBar
            }
        }
        .environment(\.colorScheme, scheme)
        // a hardware keyboard turns pages even before the page has focus
        .background {
            Group {
                Button("Next page", action: reader.next).keyboardShortcut(.rightArrow, modifiers: [])
                Button("Previous page", action: reader.prev).keyboardShortcut(.leftArrow, modifiers: [])
                Button("Close book", action: close).keyboardShortcut("w", modifiers: .command)
            }
            .opacity(0)
            .accessibilityHidden(true)
        }
    }

    private var topBar: some View {
        HStack(spacing: Lapis.Space.s2) {
            Button(action: close) {
                Image(systemName: "chevron.left")
                    .font(.body.weight(.semibold))
                    .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
            }
            .accessibilityLabel("Close book")
            .accessibilityIdentifier("reader.close")
            VStack(spacing: 1) {
                Text(reader.title)
                    .lapisFont(.cardTitle)
                    .lineLimit(1)
                if !reader.chapter.isEmpty {
                    Text(reader.chapter)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
            }
            .frame(maxWidth: .infinity)
            .accessibilityElement(children: .combine)
            Button { showContents = true } label: {
                Image(systemName: "list.bullet")
                    .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
            }
            .accessibilityLabel("Contents")
            .disabled(reader.toc.isEmpty)
            Button { showSettings = true } label: {
                Image(systemName: "textformat.size")
                    .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
            }
            .accessibilityLabel("Reading settings")
            .accessibilityIdentifier("reader.settings")
        }
        .buttonStyle(.plain)
        .foregroundStyle(inkColor)
        .padding(.horizontal, Lapis.Space.s2)
        .padding(.vertical, Lapis.Space.s1)
        .background(.bar)
        .overlay(alignment: .bottom) { Divider() }
    }

    private var bottomBar: some View {
        VStack(spacing: 6) {
            ProgressBar(value: reader.fraction, tint: Color.lapis.accent, height: 3)
            HStack {
                Text(reader.percentText).monospacedDigit().fontWeight(.semibold)
                if let left = reader.minutesLeft {
                    Text("· \(left)").foregroundStyle(inkColor.opacity(0.7))
                }
                Spacer(minLength: 0)
            }
            .font(.caption)
            .foregroundStyle(inkColor)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("reader.progress")
        }
        .padding(.horizontal, Lapis.Space.s5)
        .padding(.top, Lapis.Space.s2)
        .padding(.bottom, Lapis.Space.s1)
        .background(reader.chromeVisible ? AnyShapeStyle(.bar) : AnyShapeStyle(paperColor))
        .opacity(reader.chromeVisible ? 1 : 0.85)
    }
}

/// The book's contents: chapters, the current one marked.
private struct ReaderContents: View {
    let reader: ReaderModel
    let done: () -> Void

    var body: some View {
        NavigationStack {
            List(reader.toc) { item in
                Button {
                    reader.go(item)
                    done()
                } label: {
                    HStack {
                        Text(item.label)
                            .font(item.depth == 0 ? .body : .subheadline)
                            .foregroundStyle(Color.lapis.ink)
                            .padding(.leading, CGFloat(item.depth) * Lapis.Space.s4)
                        Spacer()
                        if item.label == reader.chapter {
                            Image(systemName: "bookmark.fill").foregroundStyle(Color.lapis.accent)
                                .accessibilityLabel("Current chapter")
                        }
                    }
                    .frame(minHeight: Lapis.Layout.minTarget)
                    .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .listRowBackground(item.label == reader.chapter ? Color.lapis.accentSoft : Color.lapis.card)
            }
            .scrollContentBackground(.hidden)
            .background(Color.lapis.bg)
            .navigationTitle("Contents")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done", action: done) }
            }
        }
    }
}

/// Paper, size, spacing, margins, pages or scroll, typeface.
private struct ReaderSettingsSheet: View {
    @Bindable var reader: ReaderModel
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section("Paper") {
                    HStack(spacing: Lapis.Space.s3) {
                        ForEach(ReaderSettings.Paper.allCases) { p in paperSwatch(p) }
                    }
                    .padding(.vertical, Lapis.Space.s1)
                }
                Section("Text") {
                    HStack {
                        Button {
                            reader.settings.fontSize = ReaderSettings.step(ReaderSettings.fontSizes, reader.settings.fontSize, -1)
                        } label: {
                            Image(systemName: "textformat.size.smaller").frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                        }
                        .disabled(!reader.settings.canShrink)
                        .accessibilityLabel("Smaller text")
                        Spacer()
                        Text("\(reader.settings.fontSize)%").monospacedDigit().foregroundStyle(Color.lapis.ink2)
                            .accessibilityLabel("Text size \(reader.settings.fontSize) percent")
                        Spacer()
                        Button {
                            reader.settings.fontSize = ReaderSettings.step(ReaderSettings.fontSizes, reader.settings.fontSize, 1)
                        } label: {
                            Image(systemName: "textformat.size.larger").frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                        }
                        .disabled(!reader.settings.canGrow)
                        .accessibilityLabel("Larger text")
                    }
                    .buttonStyle(.borderless)
                    Picker("Line spacing", selection: $reader.settings.lineHeight) {
                        ForEach(ReaderSettings.lineHeights, id: \.self) { Text(String($0)).tag($0) }
                    }
                    Picker("Typeface", selection: $reader.settings.face) {
                        ForEach(ReaderSettings.Face.allCases) { Text($0.title).tag($0) }
                    }
                }
                Section("Layout") {
                    Picker("Margins", selection: $reader.settings.margins) {
                        ForEach(ReaderSettings.Margins.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                    Picker("Turn", selection: $reader.settings.flow) {
                        ForEach(ReaderSettings.Flow.allCases) { Text($0.title).tag($0) }
                    }
                    .pickerStyle(.segmented)
                }
            }
            .scrollContentBackground(.hidden)
            .background(Color.lapis.bg)
            .navigationTitle("Reading")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
    }

    private func paperSwatch(_ p: ReaderSettings.Paper) -> some View {
        let selected = reader.settings.paper == p
        let colours = ReaderPaperColours.of(p == .auto ? reader.paper : p)
        return Button {
            Haptics.selection()
            reader.settings.paper = p
        } label: {
            VStack(spacing: 6) {
                ZStack {
                    Circle().fill(Color(hexCSS: colours.paper))
                    if p == .auto {
                        Image(systemName: "circle.lefthalf.filled").foregroundStyle(Color(hexCSS: colours.ink))
                    } else {
                        Text("Aa").font(.newsreader(15, weight: .semibold)).foregroundStyle(Color(hexCSS: colours.ink))
                    }
                }
                .frame(width: 44, height: 44)
                .overlay(Circle().strokeBorder(selected ? Color.lapis.accent : Color.lapis.line2, lineWidth: selected ? 3 : 1))
                Text(p.title).font(.caption).foregroundStyle(selected ? Color.lapis.accent : Color.lapis.ink2)
            }
            .frame(maxWidth: .infinity)
            .contentShape(.rect)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("\(p.title) paper")
        .accessibilityAddTraits(selected ? [.isSelected] : [])
    }
}

extension Color {
    /// A CSS `#rrggbb` (the reader's paper: web content, mirrored in the chrome).
    init(hexCSS: String) {
        if let c = ReaderPaperColours.rgb(hexCSS) {
            self.init(red: c.r, green: c.g, blue: c.b)
        } else {
            self = Color.lapis.bg
        }
    }
}
