import Foundation

protocol Renderable {
    func render() -> String
}

struct HomeView: Renderable {
    let title: String

    func render() -> String { return format(title) }

    private func format(_ value: String) -> String { return value.uppercased() }
}

extension HomeView {
    func subtitle() -> String { return render() + "!" }
}

enum API {
    enum DependencyController {
        struct GetRoute {
            static func query() -> String { return "deps" }
        }
    }
}

enum PackageController {
    struct GetRoute {
        static func query() -> String { return "packages" }
    }
}

final class Cache {
    var imageCachedType: Int = 0

    func store(key: String, value: Int) { imageCachedType = value }
    func store(key: String) { store(key: key, value: 0) }
}
