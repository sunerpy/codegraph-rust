import Foundation

public class Greeter: Base, Drawable {
    @Published var name: String = "hi"

    public static func make() -> Greeter? { return Greeter() }

    func greet(name: String) -> String {
        return helper(name)
    }
}

struct Point { var x: Int }

enum HTTPMethod { case put, delete }

protocol Drawable { func draw() }

typealias Alias = Greeter

extension KF.Builder {
    func build() {}
}

func topLevel() async {
    Greeter.make().greet(name: "x")
    session.request(url)
}

let answer = 42
