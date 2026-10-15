// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Compatibility wrapper for loading shared runner configuration.

use std::fs;
use std::path::Path;

use crate::{ServerError, ServerResult, ServerState};

/// Loads a TOML deployment file and constructs the complete server state.
///
/// The document is kept alongside the runner so the configuration API can
/// return what the server is running.
pub fn load_server_state(path: impl AsRef<Path>) -> ServerResult<ServerState> {
    let path = path.as_ref();
    let source = fs::read_to_string(path).map_err(|error| {
        ServerError::new(format!(
            "failed to read server config {}: {error}",
            path.display()
        ))
    })?;
    ServerState::from_toml(source).map_err(|error| {
        ServerError::new(format!("invalid server config {}: {error}", path.display()))
    })
}
