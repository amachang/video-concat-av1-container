use std::env;
use tokio;
use serde_json;
use base64::{
    Engine,
    engine::general_purpose as base64_general,
};
use warp::{
    self,
    Filter,
};

#[tokio::main]
async fn main() {
    let port = match env::var("PORT") {
        Ok(port) => match port.parse::<u16>() {
            Ok(port) => port,
            Err(err) => panic!("Port is not valid uint: {:}", err)
        }
        Err(err) => panic!("Port not set or invalid utf-8: {:}", err),
    };
    let pubsub_handler = warp::post()
        .and(warp::path("pubsub"))
        .and(warp::body::json())
        .map(|data: serde_json::Value| {
            if let Some(message) = data.get("message") {
                if let Some(message) = message.get("data") {
                    let message = base64_general::STANDARD.decode(message.as_str().unwrap_or("")).unwrap_or_default();
                    let message = String::from_utf8(message).unwrap_or_default();
                    println!("Received message: {}", message);
                };
            };
            warp::reply::json(&"OK")
        });

    warp::serve(pubsub_handler).run(([0, 0, 0, 0], port)).await;
}
