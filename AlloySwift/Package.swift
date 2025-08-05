// swift-tools-version:5.5
// The swift-tools-version declares the minimum version of Swift required to build this package.
// Swift Package: AlloySwift

import PackageDescription;

let package = Package(
    name: "AlloySwift",
    platforms: [
        .iOS(.v13),
        .macOS(.v10_15)
    ],
    products: [
        .library(
            name: "AlloySwift",
            targets: ["AlloySwift"]
        )
    ],
    dependencies: [ ],
    targets: [
        .binaryTarget(name: "AlloySwiftFrameWork", path: "./AlloySwiftFrameWork.xcframework"),
        .target(
            name: "AlloySwift",
            dependencies: [
                .target(name: "AlloySwiftFrameWork")
            ]
        ),
    ]
)