import SwiftUI
import TaisceKit
import UIKit

/// What a Mermaid or reladraw block shows: a shimmer while it renders, then the image
/// or the renderer's error.
enum DiagramPhase: Equatable {
    case rendering
    case image(UIImage)
    case failed(String)

    /// Asks the queue; the queue's errors (mermaid's message, the 5 s
    /// timeout) become the error card's text.
    static func load(_ request: DiagramRequest, queue: DiagramRenderQueue) async -> DiagramPhase {
        do throws(DiagramRenderError) {
            let data = try await queue.image(for: request)
            guard let image = UIImage(data: data, scale: request.scale) else { return .failed("The rendered diagram couldn't be read.") }
            return .image(image)
        } catch {
            return .failed(error.message)
        }
    }

    /// The width a diagram is laid out at: the column's, rounded down to
    /// 20 pt so small layout changes reuse the cached image.
    static func bucket(_ width: CGFloat) -> Int {
        max(160, Int(width / 20) * 20)
    }
}

/// A ```` ```mermaid ```` or ```` ```reladraw ```` fence, drawn on the device.
struct RenderedDiagramBlock: View {
    var kind: DiagramKind = .mermaid
    let source: String
    var queue: DiagramRenderQueue = Diagrams.queue

    @Environment(\.colorScheme) private var scheme
    @Environment(\.displayScale) private var scale
    @State private var width = 0
    @State private var phase = DiagramPhase.rendering
    @State private var showsFull = false

    var request: DiagramRequest {
        DiagramRequest(kind: kind, source: source, theme: scheme == .dark ? .dark : .light, width: width, scale: Double(scale))
    }

    var body: some View {
        content
            .frame(maxWidth: .infinity, alignment: .leading)
            .onGeometryChange(for: Int.self) { DiagramPhase.bucket($0.size.width) } action: { width = $0 }
            .task(id: width > 0 ? request : nil) {
                guard width > 0 else { return }
                let req = request
                if let hit = queue.cached(req), let image = UIImage(data: hit, scale: req.scale) {
                    phase = .image(image)
                    return
                }
                if case .image = phase {} else { phase = .rendering }
                phase = await DiagramPhase.load(req, queue: queue)
            }
            .fullScreenCover(isPresented: $showsFull) {
                if case let .image(image) = phase { DiagramFullScreen(image: image) }
            }
    }

    @ViewBuilder private var content: some View {
        switch phase {
        case .rendering:
            Shimmer().frame(height: 180).accessibilityLabel("Diagram, rendering")
        case let .image(image):
            Button { showsFull = true } label: {
                Image(uiImage: image)
                    .resizable()
                    .aspectRatio(image.size, contentMode: .fit)
                    .frame(maxWidth: image.size.width)
                    .padding(12)
                    .frame(maxWidth: .infinity)
                    .card(Theme.surface)
            }
            .buttonStyle(.plain)
            .accessibilityLabel("Diagram")
            .accessibilityHint("Opens full screen")
        case let .failed(message):
            DiagramNoticeCard(icon: "exclamationmark.triangle", title: "\(kind == .reladraw ? "Reladraw" : "Mermaid") diagram couldn't render", detail: message, tint: Theme.amber)
        }
    }
}

/// A placeholder that breathes while the diagram renders.
struct Shimmer: View {
    @State private var lit = false
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    var body: some View {
        RoundedRectangle(cornerRadius: Theme.radius, style: .continuous)
            .fill(Theme.surface2)
            .overlay {
                LinearGradient(colors: [.clear, Theme.hairline.opacity(0.9), .clear], startPoint: .leading, endPoint: .trailing)
                    .offset(x: lit ? 260 : -260)
                    .clipShape(RoundedRectangle(cornerRadius: Theme.radius, style: .continuous))
                    .opacity(reduceMotion ? 0 : 1)
            }
            .overlay(RoundedRectangle(cornerRadius: Theme.radius, style: .continuous).strokeBorder(Theme.hairline, lineWidth: 1))
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(.linear(duration: 1.3).repeatForever(autoreverses: false)) { lit = true }
            }
    }
}

/// Full screen with pinch-zoom and pan (a UIScrollView, so it feels native).
struct DiagramFullScreen: View {
    let image: UIImage
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            ZoomableImage(image: image)
                .ignoresSafeArea(edges: .bottom)
                .background(Theme.ground.ignoresSafeArea())
                .navigationTitle("Diagram")
                .navigationBarTitleDisplayMode(.inline)
                .toolbar {
                    ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
                }
        }
    }
}

struct ZoomableImage: UIViewRepresentable {
    let image: UIImage

    func makeUIView(context: Context) -> ZoomView { ZoomView(image: image) }
    func updateUIView(_ view: ZoomView, context: Context) { view.imageView.image = image }

    final class ZoomView: UIScrollView, UIScrollViewDelegate {
        let imageView: UIImageView

        init(image: UIImage) {
            imageView = UIImageView(image: image)
            super.init(frame: .zero)
            delegate = self
            maximumZoomScale = 6
            bouncesZoom = true
            showsHorizontalScrollIndicator = false
            showsVerticalScrollIndicator = false
            contentInsetAdjustmentBehavior = .never
            imageView.contentMode = .scaleAspectFit
            imageView.isAccessibilityElement = true
            imageView.accessibilityLabel = "Diagram"
            addSubview(imageView)
            let tap = UITapGestureRecognizer(target: self, action: #selector(doubleTap(_:)))
            tap.numberOfTapsRequired = 2
            addGestureRecognizer(tap)
        }

        @available(*, unavailable)
        required init?(coder: NSCoder) { fatalError() }

        private var fittedFor: CGSize = .zero

        override func layoutSubviews() {
            super.layoutSubviews()
            guard bounds.size != fittedFor, let size = imageView.image?.size, size.width > 0, size.height > 0 else {
                center()
                return
            }
            fittedFor = bounds.size
            // fit, never upscale past the image's own size at zoom 1
            let fit = min(bounds.width / size.width, bounds.height / size.height, 1)
            zoomScale = 1
            imageView.frame = CGRect(origin: .zero, size: CGSize(width: size.width * fit, height: size.height * fit))
            contentSize = imageView.frame.size
            minimumZoomScale = 1
            center()
        }

        func center() {
            let dx = max(0, (bounds.width - contentSize.width) / 2)
            let dy = max(0, (bounds.height - contentSize.height) / 2)
            contentInset = UIEdgeInsets(top: dy, left: dx, bottom: dy, right: dx)
        }

        func viewForZooming(in scrollView: UIScrollView) -> UIView? { imageView }
        func scrollViewDidZoom(_ scrollView: UIScrollView) { center() }

        @objc func doubleTap(_ g: UITapGestureRecognizer) {
            if zoomScale > 1 {
                setZoomScale(1, animated: true)
            } else {
                let p = g.location(in: imageView)
                zoom(to: CGRect(x: p.x - 60, y: p.y - 60, width: 120, height: 120), animated: true)
            }
        }
    }
}

/// The renderer for a diagram fence: charts, Mermaid and reladraw on the device, the
/// rest (D2, canvases) as a card.
struct DiagramBlock: View {
    let kind: String
    let source: String

    var body: some View {
        switch kind {
        case "vega-lite":
            ChartBlock(source: source)
        case "mermaid":
            RenderedDiagramBlock(kind: .mermaid, source: source)
        case "reladraw":
            RenderedDiagramBlock(kind: .reladraw, source: source)
        case "vega":
            DiagramNoticeCard(icon: "chart.xyaxis.line", title: "Chart type not supported on iPhone yet", detail: "Full Vega specs render in the desktop app; Vega-Lite ones render here.")
        default:
            DiagramNoticeCard(
                icon: "point.3.connected.trianglepath.dotted", title: "Diagram: open on desktop",
                detail: "\(kind == "d2" ? "D2" : kind == "canvas" ? "Canvas" : kind.capitalized) diagrams render in the desktop app for now."
            )
        }
    }
}
