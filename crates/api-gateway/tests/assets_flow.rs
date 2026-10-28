//! The split shell loads its styles, scripts, fonts and icon over
//! `/assets/{*path}`. This proves the route serves them with the types the
//! browser needs, and refuses everything that is not one of those files.

mod common;

use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use tower::ServiceExt;

/// `cargo test` runs with the package directory as the working directory, and
/// the gateway resolves `frontend/app` from its own (the repository root when
/// the binary is run). Point it at the repository's copy so these tests serve
/// the real files rather than an empty directory beside the crate.
fn use_repo_frontend() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("frontend")
        .join("app")
        .canonicalize()
        .expect("the repository's frontend must exist");
    std::env::set_var("FRONTEND_DIR", dir);
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, HeaderMap, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).expect("request"))
        .await
        .expect("the router must answer");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("the body must be readable")
        .to_bytes()
        .to_vec();
    (status, headers, bytes)
}

#[tokio::test]
async fn assets_are_served_with_the_type_the_browser_needs() {
    use_repo_frontend();
    let Some(harness) = common::Harness::new().await else {
        return;
    };
    for (uri, content_type) in [
        ("/assets/styles/tokens.css", "text/css; charset=utf-8"),
        ("/assets/ui/theme.js", "text/javascript; charset=utf-8"),
        ("/assets/fonts/inter-var.woff2", "font/woff2"),
        ("/assets/favicon.svg", "image/svg+xml"),
    ] {
        let (status, headers, body) = get(&harness.app, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri} was not served");
        assert_eq!(
            headers[header::CONTENT_TYPE].to_str().unwrap_or(""),
            content_type,
            "{uri} has the wrong content type"
        );
        assert_eq!(
            headers[header::CACHE_CONTROL].to_str().unwrap_or(""),
            "no-cache",
            "{uri} may be cached stale"
        );
        assert_eq!(
            headers["x-content-type-options"].to_str().unwrap_or(""),
            "nosniff",
            "{uri} is missing nosniff"
        );
        assert!(!body.is_empty(), "{uri} served an empty body");
    }
}

#[tokio::test]
async fn assets_refuse_everything_that_is_not_one_of_the_four_types() {
    use_repo_frontend();
    let Some(harness) = common::Harness::new().await else {
        return;
    };
    for uri in [
        "/assets/../Cargo.toml",
        "/assets/%2e%2e/%2e%2e/Cargo.toml",
        "/assets/.env",
        "/assets/Cargo.toml",
        "/assets/styles/missing.css",
    ] {
        let (status, _, _) = get(&harness.app, uri).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri} should not be served");
    }
}
