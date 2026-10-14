use axum::{routing::get, Router};

fn app() -> Router {
    Router::new().route("/", get(root)).route("/users", get(list_users))
}

async fn root() -> &'static str {
    "hello"
}

async fn list_users() -> String {
    format_users()
}

fn format_users() -> String {
    String::new()
}

fn main() {
    let _ = app();
}
