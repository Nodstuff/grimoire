import SwiftUI
import TaisceKit

/// A doc screen's share surfaces, driven by `Router.docAction` (its …
/// menu, the menu bar, a comment push): the Share link… sheet, the link
/// comments sheet, and PDF export (ask about linked images, build, then the
/// Mac's save panel or the iPhone's share sheet; the temp file goes after).
struct DocShareActions: ViewModifier {
    let docID: DocID
    /// owner or editor (ADR 0004): viewers may print but not share
    let canShare: Bool
    let pageLoaded: Bool

    @Environment(AppModel.self) private var model
    @Environment(Router.self) private var router
    @State private var sharing = false
    @State private var showingLinkComments = false
    @State private var exporting = false
    @State private var file: PDFExportFile?
    @State private var saving = false
    @State private var document: PDFDocumentFile?
    @State private var error: String?
    @State private var notes: [String] = []
    @State private var imageQuestion: ImageQuestion?
    @State private var shown: PDFExportFile?

    func body(content: Content) -> some View {
        content
            .onChange(of: router.docAction, initial: true) { _, action in handle(action) }
            .task(id: docID) {
                guard model.shareLinks.isAvailable else { return }
                try? await model.shareLinks.load(doc: docID)
            }
            .overlay(alignment: .bottom) {
                if exporting {
                    Label("Exporting PDF\u{2026}", systemImage: "doc.richtext")
                        .font(.footnote.weight(.medium))
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .glassEffect(.regular, in: .capsule)
                        .padding(.bottom, 16)
                        .accessibilityIdentifier("doc.exportingPDF")
                }
            }
            .sheet(isPresented: $sharing) { ShareLinkSheet(docID: docID) }
            .sheet(isPresented: $showingLinkComments) { linkComments }
            .modifier(PDFAlerts(error: $error, notes: $notes, waiting: file != nil))
            .confirmationDialog(imageQuestion?.title ?? "", isPresented: questionShown, titleVisibility: .visible) {
                Button(ImageQuestion.include) { imageQuestion = nil; export(fetchImages: true) }
                Button(ImageQuestion.keepLinks) { imageQuestion = nil; export(fetchImages: false) }
                Button("Cancel", role: .cancel) { imageQuestion = nil }
            } message: {
                Text(ImageQuestion.message)
            }
            #if targetEnvironment(macCatalyst)
            .fileExporter(isPresented: $saving, document: document, contentType: .pdf, defaultFilename: file?.url.lastPathComponent) { result in
                if case let .failure(e) = result { error = e.localizedDescription }
                finished()
            }
            .onChange(of: saving) { _, open in if !open { finished() } }
            #else
            .sheet(item: $file, onDismiss: { shown?.cleanUp(); shown = nil }) { f in
                ActivityView(items: [f.url]).presentationDetents([.medium, .large]).onAppear { shown = f }
            }
            #endif
    }

    private var questionShown: Binding<Bool> {
        Binding(get: { imageQuestion != nil }, set: { if !$0 { imageQuestion = nil } })
    }

    private var linkComments: some View {
        NavigationStack {
            LinkCommentsScreen(docID: docID)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { showingLinkComments = false } }
                }
        }
        .tint(Theme.accent)
    }

    private func handle(_ action: DocAction?) {
        guard let action, action.doc == docID else { return }
        router.docAction = nil
        // a request left over from a while ago (the doc wasn't open then) is dropped
        guard action.isFresh() else { return }
        switch action.kind {
        case .shareLink: if model.shareLinks.isAvailable, canShare, pageLoaded { sharing = true }
        case .exportPDF: if model.shareLinks.isAvailable { askThenExport() }
        case .linkComments: showingLinkComments = true
        }
    }

    private func askThenExport() {
        guard !exporting else { return }
        Task {
            let count = await model.linkedImageCount(docID)
            if count > 0 {
                imageQuestion = ImageQuestion(count: count, hosts: await model.linkedImageHosts(docID))
            } else {
                export(fetchImages: false)
            }
        }
    }

    private func export(fetchImages: Bool) {
        guard !exporting else { return }
        exporting = true
        Task {
            defer { exporting = false }
            do {
                let f = try await model.exportPDF(docID, fetchImages: fetchImages)
                notes = f.notes
                #if targetEnvironment(macCatalyst)
                // the panel writes from memory: the temp copy can go now
                document = PDFDocumentFile(data: try Data(contentsOf: f.url))
                f.cleanUp()
                file = f
                saving = true
                #else
                file = f
                #endif
            } catch {
                self.error = ShareErrorText.message(error)
            }
        }
    }

    /// The Mac's save panel answered or closed.
    private func finished() {
        file?.cleanUp()
        file = nil
        document = nil
    }
}

/// "Couldn't export the PDF", and the notes on one that worked (shown once
/// the save panel or share sheet has gone).
private struct PDFAlerts: ViewModifier {
    @Binding var error: String?
    @Binding var notes: [String]
    let waiting: Bool

    func body(content: Content) -> some View {
        content
            .alert("Couldn't export the PDF", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) {
                Button("OK", role: .cancel) { error = nil }
            } message: {
                Text(error ?? "")
            }
            .alert("PDF exported, with notes", isPresented: Binding(get: { !notes.isEmpty && !waiting }, set: { if !$0 { notes = [] } })) {
                Button("OK", role: .cancel) { notes = [] }
            } message: {
                Text(notes.joined(separator: "\n"))
            }
    }
}
