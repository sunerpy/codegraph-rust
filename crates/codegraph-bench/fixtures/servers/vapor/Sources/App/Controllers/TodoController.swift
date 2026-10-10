import Vapor

struct TodoController: RouteCollection {
    func boot(routes: RoutesBuilder) throws {
        let todos = routes.grouped("todos")
        todos.get(use: index)
    }

    func index(req: Request) async throws -> [String] {
        return []
    }
}
