// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Read and replace the running deployment configuration over HTTP.
//!
//! The server normally reads its TOML file once at startup. These endpoints let
//! a caller read that document, replace it, and have the change take effect
//! without a restart.
//!
//! The document stays the unit of change. A replacement is parsed and built by
//! the same code that loads the file, so the API cannot accept a configuration
//! the file loader would reject, and the two cannot drift apart.

use std::path::PathBuf;
use std::sync::Arc;

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ConfigSnapshot, ServerState};

/// Who may use the configuration API, and whether changes are written to disk.
pub struct AdminAccess {
    token: String,
    config_path: Option<PathBuf>,
}

impl AdminAccess {
    /// Requires `token` on every configuration request.
    ///
    /// The endpoints replace model routing for every caller of this server, so
    /// they stay unreachable until a token is set.
    pub fn new(token: impl Into<String>) -> Self {
        Self {
            token: token.into(),
            config_path: None,
        }
    }

    /// Writes an applied configuration back to `path`.
    ///
    /// Without this an applied configuration lives only in memory and the
    /// server returns to its file on restart.
    pub fn persisting_to(mut self, path: impl Into<PathBuf>) -> Self {
        self.config_path = Some(path.into());
        self
    }
}

/// The running configuration.
#[derive(Serialize)]
pub struct ConfigResponse {
    pub version: u64,
    /// The TOML deployment document.
    source: String,
    /// Whether an applied configuration is written back to the config file.
    persisted: bool,
}

/// A replacement configuration.
#[derive(Deserialize)]
pub struct ApplyRequest {
    /// The complete TOML deployment document. Partial documents are rejected,
    /// because the runner validates routes against targets and clients together.
    source: String,
    /// The version the caller read before editing.
    ///
    /// The apply is refused when the running version has moved on, so two
    /// callers editing at once cannot silently overwrite each other.
    #[serde(default)]
    expected_version: Option<u64>,
}

#[derive(Serialize)]
pub struct ApplyResponse {
    version: u64,
    persisted: bool,
}

/// Returns the running configuration document.
#[allow(clippy::result_large_err)]
pub(crate) async fn get_config(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<ConfigResponse>, Response> {
    let access = authorize(&state, &headers)?;
    let config = state.config();
    Ok(Json(ConfigResponse {
        version: config.version(),
        source: config.source().to_string(),
        persisted: access.config_path.is_some(),
    }))
}

/// Validates and applies a replacement configuration.
#[allow(clippy::result_large_err)]
pub(crate) async fn put_config(
    State(state): State<ServerState>,
    headers: HeaderMap,
    Json(request): Json<ApplyRequest>,
) -> Result<Json<ApplyResponse>, Response> {
    let access = authorize(&state, &headers)?;

    if let Some(expected) = request.expected_version {
        let current = state.config().version();
        if expected != current {
            return Err(error_response(
                StatusCode::CONFLICT,
                format!(
                    "configuration version is {current}, not the expected {expected}; read it again and reapply your change"
                ),
            ));
        }
    }

    // Writing before the build would leave a file the server cannot load.
    let snapshot = state
        .apply_config(request.source)
        .map_err(|error| error_response(StatusCode::BAD_REQUEST, error.to_string()))?;

    if let Some(path) = access.config_path.as_ref() {
        write_config_file(path, snapshot.source()).map_err(|error| {
            // The configuration is already serving. Saying so matters: a caller
            // that treats this as a total failure would retry an applied change.
            error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "configuration version {} is serving but was not written to {}: {error}",
                    snapshot.version(),
                    path.display()
                ),
            )
        })?;
    }

    tracing::info!(
        version = snapshot.version(),
        persisted = access.config_path.is_some(),
        "applied configuration"
    );

    Ok(Json(ApplyResponse {
        version: snapshot.version(),
        persisted: access.config_path.is_some(),
    }))
}

/// Describes each route: the models behind it, their prompts, and its algorithm.
///
/// `/v1/models` lists the route names a client may request. This answers the
/// next question, which is what a route actually does.
#[allow(clippy::result_large_err)]
pub(crate) async fn get_routes(
    State(state): State<ServerState>,
    headers: HeaderMap,
) -> Result<Json<Value>, Response> {
    authorize(&state, &headers)?;
    let config = state.config();
    Ok(Json(describe_routes(&config)))
}

/// Replaces the config file atomically.
///
/// A direct write can leave a truncated file if the process stops midway, and
/// that file is what the server reads on restart.
fn write_config_file(path: &PathBuf, source: &str) -> std::io::Result<()> {
    let directory = path.parent().unwrap_or_else(|| std::path::Path::new("."));
    let temporary = directory.join(format!(
        ".{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    std::fs::write(&temporary, source)?;
    std::fs::rename(&temporary, path)
}

/// Builds the route description from the document the runner was built from.
///
/// The document is read rather than the built runner because it still holds the
/// target names, the prompts, and the algorithm settings in the shape an
/// operator wrote them.
fn describe_routes(config: &ConfigSnapshot) -> Value {
    let document: toml::Value = match toml::from_str(config.source()) {
        Ok(document) => document,
        // The running configuration built, so it parsed. An empty description is
        // still right for state created without a document.
        Err(_) => return json!({ "version": config.version(), "routes": [] }),
    };
    let table = document.as_table();
    let targets = table
        .and_then(|table| table.get("targets"))
        .and_then(toml::Value::as_table);
    let routes = table
        .and_then(|table| table.get("routes"))
        .and_then(toml::Value::as_table);

    let described = routes
        .map(|routes| {
            routes
                .iter()
                .map(|(name, route)| describe_route(name, route, targets))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    json!({ "version": config.version(), "routes": described })
}

fn describe_route(
    name: &str,
    route: &toml::Value,
    targets: Option<&toml::map::Map<String, toml::Value>>,
) -> Value {
    let route_table = route.as_table();
    let string_field = |key: &str| {
        route_table
            .and_then(|table| table.get(key))
            .and_then(toml::Value::as_str)
            .map(str::to_string)
    };

    // Target names appear under several algorithm-specific keys. Collecting every
    // string that names a known target keeps this working as algorithms change,
    // without this module tracking each algorithm's schema.
    let mut named = Vec::new();
    if let (Some(route_table), Some(targets)) = (route_table, targets) {
        collect_target_names(route_table, targets, &mut named);
    }
    named.sort();
    named.dedup();

    let resolved = named
        .iter()
        .map(|target_name| {
            let target = targets
                .and_then(|targets| targets.get(target_name.as_str()))
                .and_then(toml::Value::as_table);
            let field = |key: &str| {
                target
                    .and_then(|target| target.get(key))
                    .and_then(toml::Value::as_str)
                    .map(str::to_string)
            };
            json!({
                "target": target_name,
                "model": field("id"),
                "llm_client": field("llm_client"),
                "system_prompt": field("system_prompt"),
            })
        })
        .collect::<Vec<_>>();

    json!({
        "route": name,
        "id": string_field("id"),
        "algorithm": string_field("type"),
        "prompt": string_field("prompt"),
        "targets": resolved,
    })
}

/// Collects every string in `table` that names an entry in `targets`.
fn collect_target_names(
    table: &toml::map::Map<String, toml::Value>,
    targets: &toml::map::Map<String, toml::Value>,
    found: &mut Vec<String>,
) {
    for value in table.values() {
        match value {
            toml::Value::String(candidate) if targets.contains_key(candidate) => {
                found.push(candidate.clone());
            }
            toml::Value::Array(values) => {
                for value in values {
                    if let Some(candidate) = value.as_str()
                        && targets.contains_key(candidate)
                    {
                        found.push(candidate.to_string());
                    }
                }
            }
            toml::Value::Table(nested) => collect_target_names(nested, targets, found),
            _ => {}
        }
    }
}

/// Checks the admin token, returning the access policy when it matches.
#[allow(clippy::result_large_err)]
fn authorize<'a>(
    state: &'a ServerState,
    headers: &HeaderMap,
) -> Result<&'a Arc<AdminAccess>, Response> {
    let Some(access) = state.admin.as_ref() else {
        return Err(error_response(
            StatusCode::NOT_FOUND,
            "the configuration API is disabled; start the server with an admin token to enable it",
        ));
    };
    let presented = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !constant_time_eq(presented.as_bytes(), access.token.as_bytes()) {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            "a valid admin token is required",
        ));
    }
    Ok(access)
}

/// Compares without returning early, so timing does not reveal the token.
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0u8, |difference, (left, right)| difference | (left ^ right))
        == 0
}

fn error_response(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}
