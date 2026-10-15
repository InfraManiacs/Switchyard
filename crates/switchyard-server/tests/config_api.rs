// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Integration tests for the configuration API.

use std::error::Error;

use axum::Router;
use axum::body::Body;
use axum::http::{Request as HttpRequest, StatusCode};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use switchyard_server::config_api::AdminAccess;
use switchyard_server::{ServerState, build_switchyard_router};
use tower::ServiceExt;

type TestError = Box<dyn Error + Send + Sync>;
type TestResult<T = ()> = Result<T, TestError>;

const TOKEN: &str = "test-admin-token";

fn config_with_route(route: &str) -> String {
    format!(
        r#"
schema_version = 1

[llm_clients.upstream]
format = "openai_chat"
base_url = "http://127.0.0.1:1/v1"

[targets.cheap]
id = "cheap-model"
llm_client = "upstream"
system_prompt = "Answer briefly."

[targets.capable]
id = "capable-model"
llm_client = "upstream"

[routes.{route}]
id = "switchyard/{route}"
type = "passthrough"
target = "cheap"
"#
    )
}

fn state() -> TestResult<ServerState> {
    Ok(ServerState::from_toml(config_with_route("first"))?
        .with_admin_access(AdminAccess::new(TOKEN)))
}

fn router() -> TestResult<Router> {
    Ok(build_switchyard_router(state()?))
}

async fn send(router: &Router, request: HttpRequest<Body>) -> TestResult<(StatusCode, Value)> {
    let response = router.clone().oneshot(request).await?;
    let status = response.status();
    let bytes = response.into_body().collect().await?.to_bytes();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)?
    };
    Ok((status, body))
}

fn get(path: &str, token: Option<&str>) -> HttpRequest<Body> {
    let mut builder = HttpRequest::builder().uri(path).method("GET");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder.body(Body::empty()).expect("request builds")
}

fn put(path: &str, token: Option<&str>, payload: Value) -> HttpRequest<Body> {
    let mut builder = HttpRequest::builder()
        .uri(path)
        .method("PUT")
        .header("content-type", "application/json");
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    builder
        .body(Body::from(payload.to_string()))
        .expect("request builds")
}

#[tokio::test]
async fn get_returns_the_running_document() -> TestResult {
    let router = router()?;
    let (status, body) = send(&router, get("/v1/config", Some(TOKEN))).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["version"], 1);
    assert!(
        body["source"]
            .as_str()
            .is_some_and(|source| source.contains("routes.first")),
        "document should be the one the server loaded: {body}"
    );
    Ok(())
}

#[tokio::test]
async fn applying_a_document_changes_the_served_routes() -> TestResult {
    let router = router()?;
    let (status, _) = send(&router, get("/v1/models", None)).await?;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(
        &router,
        put(
            "/v1/config",
            Some(TOKEN),
            json!({ "source": config_with_route("second") }),
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["version"], 2);

    // The new route is served without a restart, and the old one is gone.
    let (_, models) = send(&router, get("/v1/models", None)).await?;
    let served = models["data"]
        .as_array()
        .map(|models| {
            models
                .iter()
                .filter_map(|model| model["id"].as_str().map(str::to_string))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    assert_eq!(served, vec!["switchyard/second".to_string()], "{models}");
    Ok(())
}

#[tokio::test]
async fn a_rejected_document_leaves_the_previous_one_serving() -> TestResult {
    let router = router()?;
    let (status, body) = send(
        &router,
        put(
            "/v1/config",
            Some(TOKEN),
            json!({ "source": "schema_version = 1\n[routes.broken]\nid = \"x\"\n" }),
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");

    let (status, body) = send(&router, get("/v1/config", Some(TOKEN))).await?;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["version"], 1,
        "version should not advance on a failure"
    );
    assert!(
        body["source"]
            .as_str()
            .is_some_and(|source| source.contains("routes.first")),
        "the previous document should still be serving"
    );
    Ok(())
}

#[tokio::test]
async fn a_stale_expected_version_is_refused() -> TestResult {
    let router = router()?;
    let (status, _) = send(
        &router,
        put(
            "/v1/config",
            Some(TOKEN),
            json!({ "source": config_with_route("second"), "expected_version": 1 }),
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);

    // The second caller read version 1 before the first caller applied.
    let (status, body) = send(
        &router,
        put(
            "/v1/config",
            Some(TOKEN),
            json!({ "source": config_with_route("third"), "expected_version": 1 }),
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");

    let (_, body) = send(&router, get("/v1/config", Some(TOKEN))).await?;
    assert!(
        body["source"]
            .as_str()
            .is_some_and(|source| source.contains("routes.second")),
        "the refused apply must not overwrite the applied one"
    );
    Ok(())
}

#[tokio::test]
async fn routes_describe_their_targets_and_prompts() -> TestResult {
    let router = router()?;
    let (status, body) = send(&router, get("/v1/config/routes", Some(TOKEN))).await?;
    assert_eq!(status, StatusCode::OK);
    let route = &body["routes"][0];
    assert_eq!(route["route"], "first");
    assert_eq!(route["id"], "switchyard/first");
    assert_eq!(route["algorithm"], "passthrough");
    let target = &route["targets"][0];
    assert_eq!(target["target"], "cheap");
    assert_eq!(target["model"], "cheap-model");
    assert_eq!(target["system_prompt"], "Answer briefly.");
    Ok(())
}

#[tokio::test]
async fn a_missing_or_wrong_token_is_refused() -> TestResult {
    let router = router()?;
    for request in [
        get("/v1/config", None),
        get("/v1/config", Some("wrong-token")),
        get("/v1/config/routes", Some("wrong-token")),
    ] {
        let (status, _) = send(&router, request).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED);
    }

    let (status, _) = send(
        &router,
        put(
            "/v1/config",
            Some("wrong-token"),
            json!({ "source": config_with_route("second") }),
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    Ok(())
}

#[tokio::test]
async fn the_api_is_absent_without_an_admin_token() -> TestResult {
    let router = build_switchyard_router(ServerState::from_toml(config_with_route("first"))?);
    let (status, _) = send(&router, get("/v1/config", Some(TOKEN))).await?;
    assert_eq!(status, StatusCode::NOT_FOUND);
    Ok(())
}

#[tokio::test]
async fn applying_writes_the_file_when_persistence_is_on() -> TestResult {
    let directory = tempfile::tempdir()?;
    let path = directory.path().join("switchyard.toml");
    std::fs::write(&path, config_with_route("first"))?;

    let router = build_switchyard_router(
        ServerState::from_toml(config_with_route("first"))?
            .with_admin_access(AdminAccess::new(TOKEN).persisting_to(&path)),
    );
    let (status, body) = send(
        &router,
        put(
            "/v1/config",
            Some(TOKEN),
            json!({ "source": config_with_route("second") }),
        ),
    )
    .await?;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["persisted"], true);

    let written = std::fs::read_to_string(&path)?;
    assert!(
        written.contains("routes.second"),
        "the applied document should be on disk: {written}"
    );
    // The file must be loadable, or the next restart fails.
    ServerState::from_toml(written)?;
    Ok(())
}
