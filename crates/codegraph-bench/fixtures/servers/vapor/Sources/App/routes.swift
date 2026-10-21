import Vapor

func routes(_ app: Application) throws {
    app.get("hello") { req -> String in
        return greeting()
    }
    try app.register(collection: TodoController())
}

func greeting() -> String {
    return "hello"
}
