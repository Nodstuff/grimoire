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

    private var store: ShareLinkStore { model.shareLinks }
    private var title: String { model.index.byID[docID]?.title ?? "This doc" }

    var body: some View {
        let shares = store.shares(for: docID).filter { only == nil || $0.id == only }
        let live = shares.filter { $0.state() == .active }
        let ended = shares.filter { $0.state() != .active }
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
            ForEach(live) { share in
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
                Section(only == nil ? "Ended links" : "") {
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
            if !store.lastProblems.isEmpty {
                Section("Not everything made it") {
                    ForEach(store.lastProblems, id: \.self) { Text($0).font(.footnote).foregroundStyle(Theme.amber) }
                }
                .listRowBackground(Theme.surface)
            }
            if let error {
                Section { Text(error).font(.footnote).foregroundStyle(Theme.rose) }
                    .listRowBackground(Theme.surface)
            }
        }
        .groundBackground()
        .task(id: docID) {
            do {
                try await store.load(doc: docID)
                error = nil
            } catch {
                self.error = error.localizedDescription
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
                Task { await create() }
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

    private func create() async {
        creating = true
        defer { creating = false }
        do {
            let share = try await store.create(
                doc: docID, expiresAt: expiry.date(custom: customDate), commentsEnabled: commentsOn, theme: ShareSnapshot.Theme(scheme)
            )
            UIPasteboard.general.string = share.url
            newLink = false
            error = nil
        } catch {
            self.error = error.localizedDescription
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
            DatePicker("Expires on", selection: $customDate, in: Date.now.addingTimeInterval(60)..., displayedComponents: [.date, .hourAndMinute])
        }
    }
}

/// One live link: its URL and numbers, Copy, comments on/off, Update,
/// change expiry, Revoke (asks first).
struct ShareLinkSection: View {
    let share: Share
    let theme: ShareSnapshot.Theme
    var showComments = false
    @Environment(AppModel.self) private var model
    @State private var busy: String?
    @State private var error: String?
    @State private var copied = false
    @State private var confirmRevoke = false
    @State private var changingExpiry = false
    @State private var expiry = ShareExpiry.default
    @State private var customDate = Date.now.addingTimeInterval(14 * 86400)

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
                run("update") { try await store.republish(share, theme: theme) }
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
                        let date = expiry.date(custom: customDate)
                        run("expiry") {
                            try await store.setExpiry(share, to: date)
                            changingExpiry = false
                        }
                    }
                    .buttonStyle(.borderless)
                    .disabled(busy != nil)
                }
            } else {
                Button("Change expiry\u{2026}", systemImage: "clock") { changingExpiry = true }
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

    private func run(_ key: String, _ work: @escaping @MainActor () async throws -> Void) {
        busy = key
        Task {
            do {
                try await work()
                error = nil
            } catch {
                self.error = error.localizedDescription
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
