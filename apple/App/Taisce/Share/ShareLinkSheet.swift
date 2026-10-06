import SwiftUI
import TaisceKit
import UIKit

/// "Share link…" on a doc: make a link (expiry, comments), then copy,
/// update, change or revoke it. The doc's ended links are listed below.
struct ShareLinkSheet: View {
    let docID: DocID
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ShareLinkForm(docID: docID)
                .navigationTitle("Share link")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                }
        }
        .tint(Theme.accent)
    }
}

/// The sheet's content (also Settings › Shared links › a link, with `only`).
struct ShareLinkForm: View {
    let docID: DocID
    /// show just this link (Settings), not the doc's create form and others
    var only: String?
    @Environment(AppModel.self) private var model
    @Environment(\.colorScheme) private var scheme
    @State private var loaded = false
    @State private var error: String?
    @State private var creating = false
    @State private var newLink = false
    @State private var expiry = ShareExpiry.default
    @State private var customDate = Date.now.addingTimeInterval(14 * 86400)
    @State private var commentsOn = true
    /// the publish in flight: cancelled if the sheet closes
    @State private var publishTask: Task<Void, Never>?
    @State private var visible = false
    @State private var imageQuestion: ImageQuestion?

    private var store: ShareLinkStore { model.shareLinks }
    private var title: String { model.index.byID[docID]?.title ?? "This doc" }

    var body: some View {
        let shares = store.shares(for: docID).filter { only == nil || $0.id == only }
        let live = shares.filter { $0.state() == .active }
        // an expired link can still be updated or given a new expiry; only a revoked one is over
        let manageable = shares.filter { $0.state() != .revoked }
        let ended = shares.filter { $0.state() == .revoked }
        Form {
            if only == nil {
                Section {
                    Text(title).font(.headline).foregroundStyle(Theme.text)
                } footer: {
                    Text("Anyone with the link sees this doc as it is now: a snapshot, diagrams included. Later edits stay private until you update the link.")
                }
                .listRowBackground(Theme.surface)
            }
            if !loaded && shares.isEmpty {
                Section { ProgressView().frame(maxWidth: .infinity) }.listRowBackground(Theme.surface)
            } else if only == nil && (live.isEmpty || newLink) {
                createSection
            }
            ForEach(manageable) { share in
                ShareLinkSection(share: share, theme: ShareSnapshot.Theme(scheme), showComments: only != nil)
            }
            if only == nil, !live.isEmpty, !newLink {
                Section {
                    Button("New link\u{2026}", systemImage: "plus") { newLink = true }
                        .accessibilityIdentifier("share.newLink")
                }
                .listRowBackground(Theme.surface)
            }
            if !ended.isEmpty {
                Section(only == nil ? "Revoked links" : "") {
                    ForEach(ended) { share in
                        if only != nil {
                            EndedLinkSummary(share: share)
                        } else {
                            EndedLinkRow(share: share)
                        }
                    }
                }
                .listRowBackground(Theme.surface)
            }
            if store.unsentEdits[docID] == true {
                Section {
                    Label("Includes unsent edits: changes on this device that haven't reached the server yet.", systemImage: "arrow.up.circle")
                        .font(.footnote)
                        .foregroundStyle(Theme.amber)
                        .accessibilityIdentifier("share.unsent")
                }
                .listRowBackground(Theme.surface)
            }
            if let problems = store.problems[docID], !problems.isEmpty {
                Section("Not everything made it") {
                    ForEach(problems, id: \.self) { Text($0).font(.footnote).foregroundStyle(Theme.amber) }
                }
                .listRowBackground(Theme.surface)
            }
            if let error {
                Section { Text(error).font(.footnote).foregroundStyle(Theme.rose) }
                    .listRowBackground(Theme.surface)
            }
        }
        .groundBackground()
        .onAppear {
            visible = true
            if only == nil { store.clearNotes(for: docID) }
        }
        .onDisappear {
            visible = false
            publishTask?.cancel()
            publishTask = nil
        }
        .confirmationDialog(imageQuestion?.title ?? "", isPresented: Binding(get: { imageQuestion != nil }, set: { if !$0 { imageQuestion = nil } }), titleVisibility: .visible) {
            Button(ImageQuestion.include) { imageQuestion = nil; startCreate(fetchImages: true) }
            Button(ImageQuestion.keepLinks) { imageQuestion = nil; startCreate(fetchImages: false) }
            Button("Cancel", role: .cancel) { imageQuestion = nil }
        } message: {
            Text(ImageQuestion.message)
        }
        .task(id: docID) {
            do {
                try await store.load(doc: docID)
                error = nil
            } catch {
                self.error = ShareErrorText.message(error)
            }
            loaded = true
        }
    }

    private var createSection: some View {
        Section {
            ExpiryPicker(expiry: $expiry, customDate: $customDate)
            Toggle("Allow comments", isOn: $commentsOn)
                .accessibilityIdentifier("share.comments")
            Button {
                Task { await askThenCreate() }
            } label: {
                HStack {
                    Text(creating ? "Publishing\u{2026}" : "Create link")
                    if creating { Spacer(); ProgressView() }
                }
            }
            .disabled(creating)
            .accessibilityIdentifier("share.create")
            if newLink {
                Button("Cancel", role: .cancel) { newLink = false }
            }
        } header: {
            Text("New link")
        } footer: {
            Text("Diagrams and charts are drawn in your current appearance.")
        }
        .listRowBackground(Theme.surface)
    }

    /// Linked images on other sites: ask before fetching any.
    private func askThenCreate() async {
        guard !creating else { return }
        let hosts = await model.linkedImageHosts(docID)
        let count = await model.linkedImageCount(docID)
        if count > 0 {
            imageQuestion = ImageQuestion(count: count, hosts: hosts)
        } else {
            startCreate(fetchImages: false)
        }
    }

    private func startCreate(fetchImages: Bool) {
        publishTask?.cancel()
        creating = true
        let (expiry, custom, comments, theme) = (expiry, customDate, commentsOn, ShareSnapshot.Theme(scheme))
        publishTask = Task {
            defer { creating = false }
            do {
                let share = try await store.create(doc: docID, expiry: expiry, custom: custom, commentsEnabled: comments, theme: theme, fetchImages: fetchImages)
                // only into the clipboard if the person is still looking
                if visible, !Task.isCancelled { UIPasteboard.general.string = share.url }
                newLink = false
                error = nil
            } catch is CancellationError {
            } catch {
                self.error = ShareErrorText.message(error)
            }
        }
    }
}

/// 1 hour · 1 day · 7 days · 30 days · custom date · never.
struct ExpiryPicker: View {
    @Binding var expiry: ShareExpiry
    @Binding var customDate: Date
    var label = "Expires after"

    var body: some View {
        Picker(label, selection: $expiry) {
            ForEach(ShareExpiry.allCases) { Text($0.title).tag($0) }
        }
        .accessibilityIdentifier("share.expiry")
        if expiry == .custom {
            DatePicker("Expires on", selection: $customDate, in: Date.now.addingTimeInterval(ShareExpiry.minimumLead + 60)..., displayedComponents: [.date, .hourAndMinute])
        }
    }
}

/// One live link: its URL and numbers, Copy, comments on/off, Update,
/// change expiry, Revoke (asks first).
struct ShareLinkSection: View {
    let share: Share
    let theme: ShareSnapshot.Theme
    var showComments = false
    var now: Date = .now
    @Environment(AppModel.self) private var model
    @State private var busy: String?
    @State private var error: String?
    @State private var copied = false
    @State private var confirmRevoke = false
    @State private var changingExpiry = false
    @State private var expiry = ShareExpiry.default
    @State private var customDate = Date.now.addingTimeInterval(14 * 86400)
    @State private var current: Task<Void, Never>?
    @State private var imageQuestion: ImageQuestion?

    private var store: ShareLinkStore { model.shareLinks }

    var body: some View {
        Section {
            VStack(alignment: .leading, spacing: 6) {
                Text(share.url)
                    .font(.footnote.monospaced())
                    .foregroundStyle(Theme.text)
                    .textSelection(.enabled)
                    .lineLimit(2)
                    .accessibilityIdentifier("share.url")
                Text(ShareLinkText.state(share))
                    .font(.caption)
                    .foregroundStyle(Theme.secondary)
                Text(ShareLinkText.views(share))
                    .font(.caption)
                    .foregroundStyle(Theme.secondary)
                    .accessibilityIdentifier("share.views")
                if let updated = share.updatedAt ?? share.createdAt {
                    Text("Snapshot of \(updated.formatted(date: .abbreviated, time: .shortened))\(share.revision > 1 ? " · update \(share.revision - 1)" : "")")
                        .font(.caption)
                        .foregroundStyle(Theme.secondary)
                }
            }
            .padding(.vertical, 2)
            Button(copied ? "Copied" : "Copy link", systemImage: copied ? "checkmark" : "doc.on.doc") {
                UIPasteboard.general.string = share.url
                copied = true
                Task {
                    try? await Task.sleep(for: .seconds(2))
                    copied = false
                }
            }
            .accessibilityIdentifier("share.copy")
            Toggle("Allow comments", isOn: Binding(get: { share.commentsEnabled }, set: { on in run("comments") { try await store.setComments(share, enabled: on) } }))
                .disabled(busy != nil)
            if let comments = ShareLinkText.comments(share), showComments || share.commentCount > 0 {
                NavigationLink {
                    LinkCommentsScreen(docID: share.docID)
                } label: {
                    LabeledContent("Comments") { UnreadText(text: comments, unread: share.unreadComments) }
                }
            }
            Button {
                Task { await askThenUpdate() }
            } label: {
                busyLabel("Update link", icon: "arrow.triangle.2.circlepath", key: "update")
            }
            .disabled(busy != nil)
            .accessibilityIdentifier("share.update")
            if changingExpiry {
                ExpiryPicker(expiry: $expiry, customDate: $customDate, label: "New expiry")
                HStack {
                    Button("Cancel") { changingExpiry = false }.buttonStyle(.borderless)
                    Spacer()
                    Button("Apply") {
                        let (expiry, custom) = (expiry, customDate)
                        run("expiry") {
                            try await store.setExpiry(share, expiry: expiry, custom: custom)
                            changingExpiry = false
                        }
                    }
                    .buttonStyle(.borderless)
                    .disabled(busy != nil)
                }
            } else {
                Button("Change expiry\u{2026}", systemImage: "clock") {
                    (expiry, customDate) = ExpirySeed.seed(share.expiresAt)
                    changingExpiry = true
                }
                .accessibilityIdentifier("share.changeExpiry")
            }
            if share.state(now: now) == .expired {
                Text("This link has expired: readers see that it's gone. Give it a new expiry to open it again.")
                    .font(.caption)
                    .foregroundStyle(Theme.amber)
            }
            Button(role: .destructive) {
                confirmRevoke = true
            } label: {
                busyLabel("Revoke link\u{2026}", icon: "xmark.circle", key: "revoke")
            }
            .disabled(busy != nil)
            .accessibilityIdentifier("share.revoke")
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.rose)
            }
        }
        .listRowBackground(Theme.surface)
        .onDisappear {
            current?.cancel()
            current = nil
        }
        .confirmationDialog(imageQuestion?.title ?? "", isPresented: Binding(get: { imageQuestion != nil }, set: { if !$0 { imageQuestion = nil } }), titleVisibility: .visible) {
            Button(ImageQuestion.include) { imageQuestion = nil; update(fetchImages: true) }
            Button(ImageQuestion.keepLinks) { imageQuestion = nil; update(fetchImages: false) }
            Button("Cancel", role: .cancel) { imageQuestion = nil }
        } message: {
            Text(ImageQuestion.message)
        }
        .confirmationDialog("Revoke this link?", isPresented: $confirmRevoke, titleVisibility: .visible) {
            Button("Revoke", role: .destructive) { run("revoke") { try await store.revoke(share) } }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Anyone opening it will see that it has expired. This can't be undone; you can make a new link.")
        }
    }

    @ViewBuilder private func busyLabel(_ title: String, icon: String, key: String) -> some View {
        HStack {
            Label(title, systemImage: icon)
            if busy == key { Spacer(); ProgressView() }
        }
    }

    private func askThenUpdate() async {
        guard busy == nil else { return }
        let count = await model.linkedImageCount(share.docID)
        if count > 0 {
            imageQuestion = ImageQuestion(count: count, hosts: await model.linkedImageHosts(share.docID))
        } else {
            update(fetchImages: false)
        }
    }

    private func update(fetchImages: Bool) {
        let theme = theme
        run("update") { try await store.republish(share, theme: theme, fetchImages: fetchImages) }
    }

    private func run(_ key: String, _ work: @escaping @MainActor () async throws -> Void) {
        busy = key
        current = Task {
            do {
                try await work()
                error = nil
            } catch is CancellationError {
            } catch {
                self.error = ShareErrorText.message(error)
            }
            busy = nil
        }
    }
}

/// An expired or revoked link in the doc's sheet.
struct EndedLinkRow: View {
    let share: Share

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            Text(share.url).font(.caption.monospaced()).foregroundStyle(Theme.secondary).lineLimit(1).truncationMode(.middle)
            Text("\(ShareLinkText.state(share)) · \(ShareLinkText.views(share))")
                .font(.caption2)
                .foregroundStyle(Theme.secondary)
            if share.commentCount > 0 {
                NavigationLink("Comments (\(share.commentCount))") { LinkCommentsScreen(docID: share.docID) }
                    .font(.caption)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// Settings › a link that has ended: what it was.
struct EndedLinkSummary: View {
    let share: Share

    var body: some View {
        LabeledContent("State", value: ShareLinkText.state(share))
        LabeledContent("Views", value: ShareLinkText.views(share))
        LabeledContent("Link") { Text(share.url).font(.caption.monospaced()).textSelection(.enabled) }
        if share.commentCount > 0 {
            NavigationLink("Comments (\(share.commentCount))") { LinkCommentsScreen(docID: share.docID) }
        }
    }
}

/// "3 comments · 1 new", the new part in the accent.
struct UnreadText: View {
    let text: String
    let unread: Int

    var body: some View {
        HStack(spacing: 6) {
            Text(text).foregroundStyle(Theme.secondary)
            if unread > 0 { UnreadBadge(count: unread) }
        }
    }
}

struct UnreadBadge: View {
    let count: Int

    var body: some View {
        Text("\(count)")
            .font(.caption2.weight(.bold))
            .monospacedDigit()
            .foregroundStyle(Theme.ground)
            .padding(.horizontal, 6)
            .padding(.vertical, 1)
            .background(Theme.accent, in: Capsule())
            .accessibilityLabel("\(count) new")
    }
}

/// Change expiry starts from the link's own: its date if still ahead,
/// "never" for a link without one, else the default (an expired link).
enum ExpirySeed {
    static func seed(_ current: Date?, now: Date = .now) -> (ShareExpiry, Date) {
        guard let current else { return (.never, now.addingTimeInterval(14 * 86400)) }
        if current > now.addingTimeInterval(ShareExpiry.minimumLead) { return (.custom, current) }
        return (.default, now.addingTimeInterval(14 * 86400))
    }
}
