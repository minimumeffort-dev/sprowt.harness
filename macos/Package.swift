// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "SprowtDesktop",
    platforms: [.macOS("26.0")],
    products: [.executable(name: "SprowtDesktop", targets: ["SprowtDesktop"])],
    targets: [
        .executableTarget(name: "SprowtDesktop"),
        .testTarget(name: "SprowtDesktopTests", dependencies: ["SprowtDesktop"])
    ]
)
