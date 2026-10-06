import SwiftUI
import TaisceKit

/// A doc's comments from its share links, thread by thread: who, the text
/// they quoted, what they said. Reply as the owner, or delete. Opening
/// this marks them read.
struct LinkCommentsScreen: View {
    let docID: DocID
    @Environment(AppModel.self) private var model
    @State private var threads: [String: [ShareThread]] = [:]
    @State private var loading = true
    @State private var error: String?
    @State private var replyingTo: ReplyTarget?
    @State private var draft = ""
    @State private var sending = false
    @State private var deleting: (comment: ShareComment, share: Share)?
    @FocusState private var replyFocused: Bool

    struct ReplyTarget: Equatable {
        var share: Share
        var root: ShareComment
    }

    private var store: ShareLinkStore { model.shareLinks }

    var body: some View {
        let shares = store.shares(for: docID).filter { $0.commentCount > 0 || threads[$0.id]?.isEmpty == false }
        List {
            if loading && threads.isEmpty {
                ProgressView().frame(maxWidth: .infinity).listRowBackground(Color.clear)
            } else if shares.isEmpty {
                Text("No comments from shared links yet.")
                    .font(.footnote)
                    .foregroundStyle(Theme.secondary)
                    .listRowBackground(Theme.surface)
            }
            ForEach(shares) { share in
                Section {
                    ForEach(threads[share.id] ?? []) { thread in
                        ThreadView(
                            thread: thread,
                            onReply: { replyingTo = ReplyTarget(share: share, root: thread.root); replyFocused = true },
                            onDelete: { deleting = ($0, share) }
                        )
                        .listRowBackground(Theme.surface)
                    }
                } header: {
                    Text(header(share))
                }
            }
            if let error {
                Text(error).font(.footnote).foregroundStyle(Theme.rose).listRowBackground(Theme.surface)
            }
        }
        .scrollContentBackground(.hidden)
        .groundBackground()
        .navigationTitle("Link comments")
        .navigationBarTitleDisplayMode(.inline)
        .safeAreaInset(edge: .bottom) {
            if let target = replyingTo { replyBar(target) }
        }
        .task(id: docID) { await load() }
        .refreshable { await load() }
        .confirmationDialog(
            "Delete this comment?",
            isPresented: Binding(get: { deleting != nil }, set: { if !$0 { deleting = nil } }),
            titleVisibility: .visible
        ) {
            Button("Delete", role: .destructive) {
                if let d = deleting { Task { await delete(d.comment, on: d.share) } }
                deleting = nil
            }
            Button("Cancel", role: .cancel) { deleting = nil }
        } message: {
            Text("It goes from the shared page too.")
        }
    }

    private func header(_ share: Share) -> String {
        let made = share.createdAt.map { "Link of \($0.formatted(date: .abbreviated, time: .omitted))" } ?? "Link"
        return "\(made) · \(ShareLinkText.state(share))"
    }

    private func replyBar(_ target: ReplyTarget) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Text("Replying to \(target.root.author)").font(.caption).foregroundStyle(Theme.secondary)
                Spacer()
                Button("Cancel") { replyingTo = nil; draft = "" }.font(.caption)
            }
            HStack(alignment: .bottom) {
                TextField("Reply", text: $draft, axis: .vertical)
                    .lineLimit(1...6)
                    .textFieldStyle(.roundedBorder)
                    .focused($replyFocused)
                    .accessibilityIdentifier("linkComments.reply")
                Button {
                    Task { await send(target) }
                } label: {
                    if sending { ProgressView() } else { Image(systemName: "arrow.up.circle.fill").font(.title2) }
                }
                .disabled(sending || draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                .accessibilityLabel("Send reply")
            }
        }
        .padding(12)
        .background(Theme.surface)
    }

    private func load() async {
        do {
            try await store.load(doc: docID)
            var out: [String: [ShareThread]] = [:]
            for share in store.shares(for: docID) where share.commentCount > 0 || share.unreadComments > 0 {
                out[share.id] = ShareThread.group(try await store.comments(for: share))
            }
            threads = out
            error = nil
        } catch {
            self.error = error.localizedDescription
        }
        loading = false
    }

    private func send(_ target: ReplyTarget) async {
        sending = true
        defer { sending = false }
        do {
            let current = store.shares(for: docID).first { $0.id == target.share.id } ?? target.share
            _ = try await store.reply(to: current, body: draft, parent: target.root)
            draft = ""
            replyingTo = nil
            await load()
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func delete(_ comment: ShareComment, on share: Share) async {
        do {
            let current = store.shares(for: docID).first { $0.id == share.id } ?? share
            try await store.delete(comment, on: current)
            await load()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

/// A root comment and its replies.
struct ThreadView: View {
    let thread: ShareThread
    var onReply: () -> Void = {}
    var onDelete: (ShareComment) -> Void = { _ in }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            CommentView(comment: thread.root, onDelete: { onDelete(thread.root) })
            ForEach(thread.replies) { reply in
                CommentView(comment: reply, onDelete: { onDelete(reply) })
                    .padding(.leading, 18)
                    .overlay(alignment: .leading) { Rectangle().fill(Theme.hairline).frame(width: 2) }
            }
            Button("Reply", systemImage: "arrowshape.turn.up.left", action: onReply)
                .font(.caption)
                .buttonStyle(.borderless)
                .accessibilityIdentifier("linkComments.replyButton")
        }
        .padding(.vertical, 4)
    }
}

struct CommentView: View {
    let comment: ShareComment
    var now: Date = .now
    var onDelete: () -> Void = {}

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(spacing: 6) {
                Text(comment.author.isEmpty ? "Someone" : comment.author)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(Theme.text)
                if comment.isOwner {
                    Text("You").font(.caption2.weight(.semibold)).foregroundStyle(Theme.accent)
                        .padding(.horizontal, 5).background(Theme.accent.opacity(0.15), in: Capsule())
                }
                if let at = comment.createdAt {
                    Text(RelativeTime.string(at, now: now)).font(.caption2).foregroundStyle(Theme.secondary)
                }
                Spacer()
                Menu {
                    Button("Delete\u{2026}", systemImage: "trash", role: .destructive, action: onDelete)
                } label: {
                    Image(systemName: "ellipsis").foregroundStyle(Theme.secondary).frame(width: 28, height: 28)
                }
                .accessibilityLabel("Comment actions")
            }
            if let quote = comment.anchor?.quote, !quote.isEmpty {
                Text(quote)
                    .font(.footnote.italic())
                    .foregroundStyle(Theme.secondary)
                    .lineLimit(4)
                    .padding(.leading, 8)
                    .overlay(alignment: .leading) { Rectangle().fill(Theme.accent.opacity(0.6)).frame(width: 2) }
            }
            Text(comment.body)
                .font(.body)
                .foregroundStyle(Theme.text)
                .textSelection(.enabled)
        }
        .contextMenu {
            Button("Delete\u{2026}", systemImage: "trash", role: .destructive, action: onDelete)
        }
    }
}
