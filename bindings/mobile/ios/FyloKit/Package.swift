// swift-tools-version: 5.9
// FyloKit: FyloKit's Swift wrapper over FYLO's C ABI (ADR 0026), and the
// CFylo module of Fylo.xcframework. scripts/mobile/build-xcframework.mjs
// builds the xcframework beside this file and runs the tests on a simulator.
import PackageDescription

let package = Package(
    name: "FyloKit",
    platforms: [.iOS(.v17)],
    products: [
        .library(name: "FyloKit", targets: ["FyloKit"])
    ],
    targets: [
        .binaryTarget(name: "CFylo", path: "Fylo.xcframework"),
        .target(name: "FyloKit", dependencies: ["CFylo"]),
        .testTarget(name: "FyloKitTests", dependencies: ["FyloKit"]),
    ]
)
