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
            targets: ["Sub2OpsMacOS"]
        ),
    ],
    targets: [
        .target(
            name: "Sub2OpsMacOS",
            path: "Sources"
        ),
        .testTarget(
            name: "Sub2OpsMacOSTests",
            dependencies: ["Sub2OpsMacOS"],
            path: "Tests"
        ),
    ]
)
