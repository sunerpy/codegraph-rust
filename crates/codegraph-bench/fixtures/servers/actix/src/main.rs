use actix_web::{get, App, HttpResponse, HttpServer};

#[get("/ping")]
async fn ping() -> HttpResponse {
    HttpResponse::Ok().body(pong())
}

fn pong() -> String {
    "pong".to_string()
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    HttpServer::new(|| App::new().service(ping)).bind(("127.0.0.1", 8080))?.run().await
}
