package expo.modules.hello

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition

class HelloModule : Module() {
  override fun definition() = ModuleDefinition {
    Name("Hello")

    Function("hello") { name: String ->
      greet(name)
    }
  }
}

fun greet(name: String): String = "Hello $name"
