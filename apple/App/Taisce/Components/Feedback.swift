import SwiftUI

// MARK: - Toast

/// A passing note: "Moved to Work", "Link copied", "Couldn't save · retrying".
struct ToastMessage: Identifiable, Equatable {
    enum Tone: Sendable { case neutral, success, warning, error }

    let id = UUID()
    var text: String
    var tone: Tone = .neutral
    /// defaults by tone (checkmark, exclamation…)
    var symbol: String?
    var actionTitle: String?
    var action: (@MainActor () -> Void)?
    /// seconds on screen (longer with an action)
    var duration: Double?

    static func == (a: ToastMessage, b: ToastMessage) -> Bool { a.id == b.id }

    var resolvedSymbol: String {
        if let symbol { return symbol }
        return switch tone {
        case .neutral: "info.circle"
        case .success: "checkmark.circle.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .error: "xmark.octagon.fill"
        }
    }

    var lapisTone: LapisTone {
        switch tone {
        case .neutral: .accent
        case .success: .green
        case .warning: .yellow
        case .error: .red
        }
    }
}

/// The toast itself: a floating capsule on the palette surface.
struct LapisToast: View {
    let message: ToastMessage
    var onDismiss: () -> Void = {}

    var body: some View {
        HStack(spacing: 10) {
            Image(systemName: message.resolvedSymbol)
                .foregroundStyle(message.lapisTone.mark)
                .accessibilityHidden(true)
            Text(message.text)
                .font(.footnote.weight(.medium))
                .foregroundStyle(Color.lapis.ink)
                .lineLimit(2)
            if let title = message.actionTitle, let action = message.action {
                Button(title) {
                    action()
                    onDismiss()
                }
                .font(.footnote.weight(.semibold))
                .foregroundStyle(Color.lapis.accent)
                .frame(minHeight: Lapis.Layout.minTarget)
                .buttonStyle(.plain)
            }
        }
        .padding(.leading, 14)
        .padding(.trailing, message.actionTitle == nil ? 16 : 10)
        .frame(minHeight: Lapis.Layout.minTarget)
        .background(Color.lapis.pal, in: .capsule)
        .overlay(Capsule().strokeBorder(Color.lapis.line, lineWidth: 1))
        .lapisShadow(.pop)
        .contentShape(.capsule)
        .onTapGesture(perform: onDismiss)
        .accessibilityElement(children: .combine)
        .accessibilityAddTraits(.isStaticText)
        .accessibilityHint(message.action == nil ? "Dismisses" : "")
    }
}

extension View {
    /// Shows `toast` at the top of this view, announces it, gives the
    /// matching haptic, and clears it after its duration (4 s; 7 s with
    /// an action). Set it again to replace.
    func lapisToast(_ toast: Binding<ToastMessage?>) -> some View {
        modifier(ToastPresenter(toast: toast))
    }
}

private struct ToastPresenter: ViewModifier {
    @Binding var toast: ToastMessage?

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .top) {
                if let message = toast {
                    LapisToast(message: message) { toast = nil }
                        .padding(.horizontal, Lapis.Layout.gutter)
                        .padding(.top, 8)
                        .transition(.move(edge: .top).combined(with: .opacity))
                        .task(id: message.id) {
                            AccessibilityNotification.Announcement(message.text).post()
                            try? await Task.sleep(for: .seconds(message.duration ?? (message.action == nil ? 4 : 7)))
                            if toast?.id == message.id { toast = nil }
                        }
                        .sensoryFeedback(trigger: message.id) { _, _ in
                            switch message.tone {
                            case .success: .success
                            case .warning: .warning
                            case .error: .error
                            case .neutral: nil
                            }
                        }
                }
            }
            .animation(Lapis.Motion.arrive, value: toast)
    }
}

// MARK: - Banner

/// A standing note across the top of a screen or card: "You're offline;
/// edits wait in the outbox", "This doc is shared with 2 people".
struct LapisBanner: View {
    let text: String
    var tone: LapisTone = .accent
    var symbol: String?
    var actionTitle: String?
    var action: (() -> Void)?
    var onDismiss: (() -> Void)?

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 10) {
            if let symbol {
                Image(systemName: symbol).foregroundStyle(tone.mark).accessibilityHidden(true)
            }
            Text(text)
                .font(.footnote)
                .foregroundStyle(Color.lapis.ink)
                .frame(maxWidth: .infinity, alignment: .leading)
            if let actionTitle, let action {
                Button(actionTitle, action: action).buttonStyle(.lapis(.ghost, size: .small))
            }
            if let onDismiss {
                Button(action: onDismiss) {
                    Image(systemName: "xmark").font(.caption.weight(.semibold)).foregroundStyle(Color.lapis.ink3)
                        .frame(width: Lapis.Layout.minTarget, height: Lapis.Layout.minTarget)
                        .contentShape(.rect)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Dismiss")
            }
        }
        .padding(.leading, 14)
        .padding(.trailing, onDismiss == nil ? 14 : 0)
        .frame(minHeight: Lapis.Layout.minTarget)
        .background(tone.soft, in: .rect(cornerRadius: Lapis.Radius.field, style: .continuous))
        .accessibilityElement(children: .contain)
    }
}

// MARK: - Skeleton

/// A placeholder that shimmers while something loads (a static wash with
/// Reduce Motion). `SkeletonBlock()` for a card, `SkeletonLines()` for text.
struct SkeletonBlock: View {
    var height: CGFloat? = nil
    var radius: CGFloat = Lapis.Radius.card

    var body: some View {
        RoundedRectangle(cornerRadius: radius, style: .continuous)
            .fill(Color.lapis.hover)
            .frame(height: height)
            .shimmering()
            .clipShape(RoundedRectangle(cornerRadius: radius, style: .continuous))
            .accessibilityLabel("Loading")
    }
}

/// Lines of text yet to arrive, the last one short.
struct SkeletonLines: View {
    var count = 3
    var lineHeight: CGFloat = 12

    var body: some View {
        VStack(alignment: .leading, spacing: lineHeight * 0.8) {
            ForEach(0..<count, id: \.self) { i in
                Capsule()
                    .fill(Color.lapis.hover)
                    .frame(height: lineHeight)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .scaleEffect(x: i == count - 1 ? 0.6 : (i % 2 == 0 ? 1 : 0.92), anchor: .leading)
            }
        }
        .shimmering()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("Loading")
    }
}

extension View {
    /// A light sweeping across, masked to this view's shape.
    func shimmering(_ active: Bool = true) -> some View {
        modifier(Shimmering(active: active))
    }
}

private struct Shimmering: ViewModifier {
    let active: Bool
    @State private var phase: CGFloat = -1
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            .overlay {
                if active && !reduceMotion {
                    GeometryReader { geo in
                        LinearGradient(colors: [.clear, Color.lapis.card.opacity(0.55), .clear], startPoint: .leading, endPoint: .trailing)
                            .frame(width: geo.size.width * 0.6)
                            .offset(x: phase * geo.size.width * 1.3)
                    }
                    .mask(content)
                    .allowsHitTesting(false)
                }
            }
            .onAppear {
                guard active, !reduceMotion else { return }
                withAnimation(.linear(duration: 1.4).repeatForever(autoreverses: false)) { phase = 1.3 }
            }
    }
}

// MARK: - Progress

/// A thin reading-progress bar (0…1).
struct ProgressBar: View {
    let value: Double
    var tint: Color = Color.lapis.accent
    var height: CGFloat = 4
    /// what it measures, for VoiceOver
    var label = "Reading progress"

    var body: some View {
        let v = min(max(value.isFinite ? value : 0, 0), 1)
        GeometryReader { geo in
            ZStack(alignment: .leading) {
                Capsule().fill(Color.lapis.line2)
                Capsule().fill(tint).frame(width: max(v > 0 ? height : 0, geo.size.width * v))
            }
        }
        .frame(height: height)
        .animation(Lapis.Motion.standard, value: v)
        .accessibilityElement()
        .accessibilityLabel(label)
        .accessibilityValue("\(Int((v * 100).rounded())) percent")
    }
}

#Preview("Feedback · light") {
    FeedbackGallery().preferredColorScheme(.light)
}

#Preview("Feedback · dark") {
    FeedbackGallery().preferredColorScheme(.dark)
}

private struct FeedbackGallery: View {
    @State private var toast: ToastMessage?

    var body: some View {
        LapisPreview(title: "Feedback") {
            LapisToast(message: ToastMessage(text: "Moved to Work", tone: .success))
            LapisToast(message: ToastMessage(text: "Couldn't save · retrying in 5 s", tone: .error, actionTitle: "Retry", action: {}))
            LapisBanner(text: "You're offline. Edits wait in the outbox.", tone: .yellow, symbol: "wifi.slash", onDismiss: {})
            LapisBanner(text: "Shared with 2 people", tone: .accent, symbol: "person.2", actionTitle: "Manage", action: {})
            SkeletonBlock(height: 90)
            SkeletonLines()
            ProgressBar(value: 0.42)
            Button("Show a toast") { toast = ToastMessage(text: "Link copied", tone: .success) }.buttonStyle(.lapis(.secondary))
        }
        .lapisToast($toast)
    }
}
