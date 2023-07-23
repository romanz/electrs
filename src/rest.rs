use std::sync::Arc;

use anyhow::Result;
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use tower_http::cors::CorsLayer;

use crate::config::Config;

#[axum::debug_handler]
async fn get_blocks_tip_hash() -> Result<impl IntoResponse, AppError> {
    Ok(StatusCode::OK)
}

pub async fn serve(config: Arc<Config>) -> Result<()> {
    let app = Router::new()
        .route("/blocks/tip/hash", get(get_blocks_tip_hash))
        .layer(CorsLayer::permissive());

    info!(
        "bitmaskd REST server successfully running at {}",
        config.http_addr
    );

    axum::Server::bind(&config.http_addr)
        .serve(app.into_make_service())
        .await?;

    Ok(())
}

struct AppError(anyhow::Error);

// Tell axum how to convert `AppError` into a response.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Something went wrong: {}", self.0),
        )
            .into_response()
    }
}

// This enables using `?` on functions that return `Result<_, anyhow::Error>` to turn them into
// `Result<_, AppError>`. That way you don't need to do that manually.
impl<E> From<E> for AppError
where
    E: Into<anyhow::Error>,
{
    fn from(err: E) -> Self {
        Self(err.into())
    }
}
