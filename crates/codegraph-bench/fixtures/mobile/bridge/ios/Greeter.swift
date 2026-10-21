import Foundation

@objc class Greeter: NSObject {
    @objc func greet(_ name: String) -> String {
        return "Hello \(name)"
    }
}
