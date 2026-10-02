// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "TaisceKit",
    platforms: [.iOS(.v26), .macOS(.v26), .macCatalyst(.v26)],
    products: [
        .library(name: "TaisceKit", targets: ["TaisceKit"]),
    ],
    dependencies: [
        // versions verified against GitHub releases on 2026-10-01
        .package(url: "https://github.com/groue/GRDB.swift.git", from: "7.11.1"),
        .package(url: "https://github.com/swiftlang/swift-markdown.git", from: "0.9.0"),
        .package(url: "https://github.com/square/Valet.git", from: "5.1.1"),
    ],
    targets: [
        .target(
            name: "TaisceKit",
            dependencies: [
                .product(name: "GRDB", package: "GRDB.swift"),
                .product(name: "Markdown", package: "swift-markdown"),
                .product(name: "Valet", package: "Valet"),
            ],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "TaisceKitTests",
            dependencies: ["TaisceKit"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)
