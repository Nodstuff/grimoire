// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "GrimoireKit",
    platforms: [.iOS(.v26), .macOS(.v26), .macCatalyst(.v26)],
    products: [
        .library(name: "GrimoireKit", targets: ["GrimoireKit"]),
    ],
    dependencies: [
        // versions verified against GitHub releases on 2026-10-01
        .package(url: "https://github.com/groue/GRDB.swift.git", from: "7.11.1"),
        .package(url: "https://github.com/swiftlang/swift-markdown.git", from: "0.9.0"),
    ],
    targets: [
        .target(
            name: "GrimoireKit",
            dependencies: [
                .product(name: "GRDB", package: "GRDB.swift"),
                .product(name: "Markdown", package: "swift-markdown"),
            ],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "GrimoireKitTests",
            dependencies: ["GrimoireKit"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ]
)
