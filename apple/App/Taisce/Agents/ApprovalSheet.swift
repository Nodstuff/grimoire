import SwiftUI
import TaisceKit

/// A guardrail stopped a tool call and the session waits on you (Slice 3).
/// What is shown is the runtime's signed request, verified against the key
/// read from the Mac's pairing code, never the server's copy; what you
/// allow is exactly those bytes (the order carries their hash), once,
/// after Face ID. The countdown is the runtime's own deadline: after it the
/// call is refused.
struct ApprovalSheet: View {
    let itemID: String
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var working: String?
    @State private var failure: String?
    @State private var reason = ""
    @State private var askingWhy = false
    @FocusState private var whyFocused: Bool

    private var store: FleetStore { model.fleet }
    private var item: FabricAttentionItem? { store.fleet.attention.first { $0.id == itemID } }

    var body: some View {
        NavigationStack {
            Group {
                if let item {
                    TimelineView(.periodic(from: .now, by: 1)) { context in
                        content(item, now: context.date)
                    }
                } else {
                    EmptyState(symbol: "checkmark.circle", title: "Dealt with", message: "This request is no longer waiting: answered, withdrawn or timed out.")
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .accessibilityIdentifier("approval.gone")
                }
            }
            .groundBackground()
            .navigationTitle("Approval")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Close") { dismiss() } }
            }
        }
    }

    private func content(_ item: FabricAttentionItem, now: Date) -> some View {
        let session = store.fleet.session(item.session)
        let verified = store.command.verify(item)
        return ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                VStack(alignment: .leading, spacing: 4) {
                    Text(item.address).font(.system(.title3, design: .monospaced).weight(.semibold)).foregroundStyle(Color.lapis.ink)
                    Text([session?.account, session?.branch, "stopped at your guardrail"].compactMap { $0?.isEmpty == false ? $0 : nil }.joined(separator: " \u{00B7} "))
                        .font(.subheadline).foregroundStyle(Color.lapis.ink3)
                }
                switch verified {
                case .success(let v):
                    verifiedBody(v, session: session, now: now)
                case .failure(let p):
                    unverified(p)
                }
                if let failure, !failure.isEmpty {
                    Text(failure).font(.footnote).foregroundStyle(Color.lapis.red).accessibilityIdentifier("approval.failure")
                }
            }
            .padding(Lapis.Layout.gutter)
            .frame(maxWidth: 560)
            .frame(maxWidth: .infinity)
        }
    }

    @ViewBuilder
    private func verifiedBody(_ v: VerifiedAttention, session: FleetSession?, now: Date) -> some View {
        let r = v.request
        let expired = r.expiresAt > 0 && Double(r.expiresAt) <= now.timeIntervalSince1970
        VStack(alignment: .leading, spacing: 10) {
            Text(["WANTS TO RUN", r.guardrailLabel.isEmpty ? nil : r.guardrailLabel.uppercased()].compactMap { $0 }.joined(separator: " \u{00B7} "))
                .font(.caption.weight(.semibold)).tracking(0.8).foregroundStyle(Color.lapis.yellow)
            Text(r.text)
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(Color.lapis.ink)
                .textSelection(.enabled)
                .padding(14)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color.lapis.code, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
                .accessibilityIdentifier("approval.command")
            let context = [r.tool.isEmpty ? nil : "Tool: \(r.tool)", session.map { s in s.lastLine.isEmpty ? nil : "Its last words: \u{201C}\(s.lastLine)\u{201D}" } ?? nil]
                .compactMap { $0 }
            if !context.isEmpty {
                Text(context.joined(separator: "\n")).font(.footnote).foregroundStyle(Color.lapis.ink2)
            }
            HStack(spacing: 6) {
                Image(systemName: "checkmark.seal.fill").foregroundStyle(Color.lapis.green)
                Text("Signed by your Mac").font(.caption).foregroundStyle(Color.lapis.ink3)
                Spacer(minLength: 0)
                if r.expiresAt > 0 {
                    Text(expired ? "Expired: it was refused" : "Expires in \(FabricWords.countdown(to: Int(r.expiresAt), now: now)) \u{00B7} then it is refused")
                        .font(.caption.weight(.semibold)).monospacedDigit()
                        .foregroundStyle(expired ? Color.lapis.red : Color.lapis.ink2)
                        .accessibilityIdentifier("approval.expiry")
                }
            }
        }
        .padding(16)
        .background(Color.lapis.yellowSoft, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))

        VStack(spacing: 10) {
            Button {
                run("allow") { await store.command.approve(v) }
            } label: {
                Label(working == "allow" ? "Allowing\u{2026}" : "Allow once with \(DeviceOwnerCheck.buttonWord)", systemImage: "faceid")
                    .frame(maxWidth: .infinity)
            }
            .buttonStyle(.lapis(.primary, size: .large, fullWidth: true))
            .disabled(working != nil || expired)
            .accessibilityIdentifier("approval.allow")
            Button {
                run("deny") { await store.command.deny(v) }
            } label: {
                Text(working == "deny" ? "Denying\u{2026}" : "Deny").frame(maxWidth: .infinity)
            }
            .buttonStyle(.lapis(.secondary, size: .large, fullWidth: true))
            .disabled(working != nil || expired)
            .accessibilityIdentifier("approval.deny")
            if askingWhy {
                TextField("Tell it why", text: $reason, axis: .vertical)
                    .lineLimit(1...4)
                    .focused($whyFocused)
                    .padding(12)
                    .background(Color.lapis.card, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: Lapis.Radius.field, style: .continuous).strokeBorder(Color.lapis.line2))
                    .accessibilityIdentifier("approval.reason")
                Button("Deny and send") { run("why") { await store.command.deny(v, because: reason) } }
                    .buttonStyle(.lapis(.destructive))
                    .disabled(reason.trimmingCharacters(in: .whitespaces).isEmpty || working != nil)
                    .accessibilityIdentifier("approval.denyWhy")
            } else {
                Button("Deny and tell it why\u{2026}") { askingWhy = true; whyFocused = true }
                    .buttonStyle(.lapis(.ghost))
                    .disabled(working != nil || expired)
            }
        }
    }

    private func unverified(_ p: FabricOrderProblem) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            Label("Not shown", systemImage: "exclamationmark.shield").font(.headline).foregroundStyle(Color.lapis.red)
            Text(FabricWords.problem(p)).font(.subheadline).foregroundStyle(Color.lapis.ink)
            Text("Nothing is allowed from this phone unless your Mac signed it. Deal with it at the Mac, or let it time out (it is then refused).")
                .font(.footnote).foregroundStyle(Color.lapis.ink3)
        }
        .padding(16)
        .background(Color.lapis.redSoft, in: .rect(cornerRadius: Lapis.Radius.card, style: .continuous))
        .accessibilityIdentifier("approval.unverified")
    }

    private func run(_ what: String, _ action: @escaping @MainActor () async -> String?) {
        working = what
        failure = nil
        Task {
            let result = await action()
            working = nil
            if let result {
                // "" is Face ID cancelled: stay, say nothing
                failure = result
            } else {
                Haptics.success()
                dismiss()
            }
        }
    }
}
