import ExpoModulesCore

public class HelloModule: Module {
  public func definition() -> ModuleDefinition {
    Name("Hello")

    Function("hello") { (name: String) -> String in
      return greet(name)
    }
  }
}

func greet(_ name: String) -> String {
  return "Hello \(name)"
}
