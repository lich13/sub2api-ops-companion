// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "Sub2OpsMacOS",
    platforms: [
        .macOS(.v14),
    ],
    products: [
        .library(
            name: "Sub2OpsMacOS",
            type: .static,
            targets: ["Sub2OpsMacOS"]
        ),
    ],
    dependencies: [.package(url: "https://github.com/jpsim/Yams.git", from: "6.2.2")],
    targets: [
        .plugin(name: "EmbedAccountCatalog", capability: .buildTool()),
        .target(
            name: "Sub2OpsMacOS",
            dependencies: ["Yams"],
            path: "Sources",
            plugins: ["EmbedAccountCatalog"]
        ),
        .testTarget(
            name: "Sub2OpsMacOSTests",
            dependencies: ["Sub2OpsMacOS"],
            path: "Tests"
        ),
    ]
)
