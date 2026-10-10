import Foundation

final class Client {
    let cache = Cache()

    func run() -> String {
        let view = HomeView(title: "home")
        cache.store(key: "a")
        cache.store(key: "b", value: 2)
        let smallest = min(1, 2)
        _ = smallest
        return API.DependencyController.GetRoute.query() + view.subtitle()
    }
}

#Preview {
    HomeView(title: "preview")
}
