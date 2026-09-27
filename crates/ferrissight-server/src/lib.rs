use axum::{routing::get, Json, Router};
use ferrissight_core::CameraId;
use serde::Serialize;

/// Public API allowlist: no user-provided name or endpoint information.
#[derive(Serialize)]
pub struct CameraSummary {
    pub id: CameraId,
}

#[derive(Serialize)]
struct Health {
    status: &'static str,
    service: &'static str,
}

pub fn app() -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async {
                Json(Health {
                    status: "ok",
                    service: "ferrissight",
                })
            }),
        )
        // Phase 0 has no camera discovery, registration, or persistence yet.
        .route(
            "/api/v1/cameras",
            get(|| async { Json(Vec::<CameraSummary>::new()) }),
        )
}

/// Serve an already-bound listener with caller-controlled graceful shutdown.
pub async fn serve(
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    axum::serve(listener, app())
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{to_bytes, Body},
        http::Request,
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn public_routes_return_only_safe_fields() {
        for (path, expected) in [
            ("/health", r#"{"status":"ok","service":"ferrissight"}"#),
            ("/api/v1/cameras", "[]"),
        ] {
            let response = app()
                .oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(body.as_ref(), expected.as_bytes());
        }
    }
}
