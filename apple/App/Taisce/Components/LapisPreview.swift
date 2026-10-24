import SwiftUI

/// A component on the Lapis ground, for previews: `#Preview { LapisPreview { VerdictChip(.green) } }`
/// and the same with `.preferredColorScheme(.dark)`.
struct LapisPreview<Content: View>: View {
    var title: String?
    @ViewBuilder var content: Content

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 16) {
                if let title { SectionLabel(title) }
                content
            }
            .padding(Lapis.Layout.gutter)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .background(Color.lapis.bg.ignoresSafeArea())
    }
}
