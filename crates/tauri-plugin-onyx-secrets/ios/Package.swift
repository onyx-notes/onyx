// swift-tools-version:5.5
import PackageDescription

let package = Package(
    name: "tauri-plugin-onyx-secrets",
    platforms: [
        .iOS(.v13)
    ],
    products: [
        .library(
            name: "tauri-plugin-onyx-secrets",
            type: .static,
            targets: ["tauri-plugin-onyx-secrets"])
    ],
    dependencies: [
        .package(name: "Tauri", path: "../.tauri/tauri-api")
    ],
    targets: [
        .target(
            name: "tauri-plugin-onyx-secrets",
            dependencies: [
                .byName(name: "Tauri")
            ],
            path: "Sources")
    ]
)
