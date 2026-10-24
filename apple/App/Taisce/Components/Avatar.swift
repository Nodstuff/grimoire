import SwiftUI

/// Who wrote it. People are plain names; agents are `kind:name`
/// principals (`claude:docs`, `keeper:API`).
enum Principal {
    static func isAgent(_ name: String, kind: String? = nil) -> Bool {
        if let kind { return !["human", "person", "user"].contains(kind.lowercased()) }
        return name.contains(":")
    }

    static func initial(_ name: String) -> String {
        let t = name.trimmingCharacters(in: .whitespaces)
        return t.first.map { String($0).uppercased() } ?? "?"
    }
}

/// A person is a round lapis avatar with their initial; an agent is a
/// violet rounded square with the sparkle.
struct LapisAvatar: View {
    let name: String
    /// force person/agent; otherwise inferred from the name
    var agent: Bool?
    var size: CGFloat = 24
    @ScaledMetric(relativeTo: .caption) private var scale: CGFloat = 1

    var isAgent: Bool { agent ?? Principal.isAgent(name) }

    var body: some View {
        let s = size * min(scale, 1.6)
        Group {
            if isAgent {
                RoundedRectangle(cornerRadius: s * 0.29, style: .continuous)
                    .fill(Color.lapis.agentSoft)
                    .overlay {
                        Image(systemName: "sparkle")
                            .font(.system(size: s * 0.52, weight: .semibold))
                            .foregroundStyle(Color.lapis.agent)
                    }
            } else {
                Circle()
                    .fill(Color.lapis.accentSoft)
                    .overlay {
                        Text(Principal.initial(name))
                            .font(.system(size: s * 0.46, weight: .semibold))
                            .foregroundStyle(Color.lapis.accent)
                    }
            }
        }
        .frame(width: s, height: s)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(isAgent ? "\(name), agent" : name)
    }
}

/// An avatar and the name: an agent's in SF Mono violet, a person's in
/// the interface face. `role` adds a quiet "person · agent" style note.
struct AgentBadge: View {
    let name: String
    var agent: Bool?
    var size: CGFloat = 20
    var note: String?

    var isAgent: Bool { agent ?? Principal.isAgent(name) }

    var body: some View {
        HStack(spacing: 7) {
            LapisAvatar(name: name, agent: isAgent, size: size)
            Text(name)
                .font(isAgent ? .lapisMono(.footnote, weight: .medium) : .footnote.weight(.medium))
                .foregroundStyle(isAgent ? Color.lapis.agent : Color.lapis.ink)
                .lineLimit(1)
                .truncationMode(.middle)
            if let note {
                Text(note).font(.caption).foregroundStyle(Color.lapis.ink3).lineLimit(1)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel([isAgent ? "\(name), agent" : name, note].compactMap { $0 }.joined(separator: ", "))
    }
}

#Preview("Who wrote it · light") {
    LapisPreview(title: "Who wrote it") {
        HStack(spacing: 12) {
            LapisAvatar(name: "Tom")
            LapisAvatar(name: "claude:docs")
            LapisAvatar(name: "Aoife", size: 36)
            LapisAvatar(name: "keeper:API", size: 36)
        }
        AgentBadge(name: "Tom", note: "2 h ago")
        AgentBadge(name: "claude:docs (Aoife)", note: "edited 5 min ago")
    }
    .preferredColorScheme(.light)
}

#Preview("Who wrote it · dark") {
    LapisPreview(title: "Who wrote it") {
        HStack(spacing: 12) {
            LapisAvatar(name: "Tom")
            LapisAvatar(name: "claude:docs")
        }
        AgentBadge(name: "Tom")
        AgentBadge(name: "claude:docs (Aoife)")
    }
    .preferredColorScheme(.dark)
}
