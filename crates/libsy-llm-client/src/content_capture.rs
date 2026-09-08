// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Opt-in capture of prompt and completion content as OpenTelemetry GenAI attributes.
//!
//! # Operator opt-in, and why it is off by default
//!
//! Nothing here records anything unless the operator sets
//! `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT` to `true` or `1` — the
//! variable named by the OpenTelemetry GenAI semantic conventions for exactly this
//! switch. Unset, or any other value, means off.
//!
//! Turning it on writes the full content of every model call into exported spans:
//! prompts, system instructions, completions, tool arguments, and tool results. That
//! content routinely carries end-user personal data, proprietary business material,
//! and text whose licence does not permit reuse. Enabling it is therefore a
//! deliberate decision about the trace backend, its retention, and who can read it —
//! not a verbosity toggle that happens to be safe by default.
//!
//! The variable is read once per process, the first time a call is observed.
//!
//! # Serialized shape
//!
//! The GenAI conventions model these attributes as structured values, but the
//! OpenTelemetry attribute types have no nested form, so each is recorded as a JSON
//! string. The shapes follow the convention's message model:
//!
//! - `gen_ai.input.messages`: `[{"role": "user", "parts": [<part>, ...]}, ...]`
//! - `gen_ai.output.messages`: the same, with a `"finish_reason"` on each message
//! - `gen_ai.system_instructions`: a flat `[<part>, ...]` list
//!
//! A part is `{"type": "text", "content": "..."}` for text, with `reasoning` and
//! `refusal` following the same shape, `{"type": "tool_call", "id", "name",
//! "arguments"}` for a tool invocation, and `{"type": "tool_call_response", "id",
//! "response"}` for its result. A media or file block records its kind and, when the
//! source is a URL, that URL — inline bytes are deliberately dropped, because a
//! base64 payload would dominate the span and is not the text this capture exists to
//! collect.
//!
//! The GenAI conventions are still marked development-stability and their message
//! shape has changed more than once, so treat these as the convention's shape as of
//! this implementation rather than a frozen contract.

use std::sync::OnceLock;

use serde_json::{Value, json};
use switchyard_protocol::{
    ContentBlock, FileSource, ImageSource, LlmRequest, MediaSource, ResponseOutput, Role,
};

/// Operator switch enabling prompt and completion capture. See the module docs.
const CAPTURE_ENV: &str = "OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT";

/// Whether the operator opted in to capturing message content.
///
/// Resolved from the environment once per process; later changes are ignored so a
/// single request cannot be observed under two different policies.
pub(crate) fn enabled() -> bool {
    static CAPTURE: OnceLock<bool> = OnceLock::new();
    *CAPTURE.get_or_init(|| std::env::var(CAPTURE_ENV).is_ok_and(|value| is_true(&value)))
}

fn is_true(value: &str) -> bool {
    matches!(value.trim().to_ascii_lowercase().as_str(), "true" | "1")
}

/// `gen_ai.input.messages` for a request, or `None` when it carries no messages.
pub(crate) fn input_messages(request: &LlmRequest) -> Option<String> {
    if request.messages.is_empty() {
        return None;
    }
    let messages = request
        .messages
        .iter()
        .map(|message| {
            json!({
                "role": role_name(message.role),
                "parts": parts(&message.content),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&messages).ok()
}

/// `gen_ai.system_instructions` for a request, or `None` when it has no instructions.
///
/// The convention models system instructions as one flat part list, so blocks from
/// several instruction entries concatenate in request order.
pub(crate) fn system_instructions(request: &LlmRequest) -> Option<String> {
    let parts = request
        .instructions
        .iter()
        .flat_map(|instruction| parts(&instruction.content))
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    serde_json::to_string(&parts).ok()
}

/// `gen_ai.output.messages` for a response, or `None` when it produced no output.
pub(crate) fn output_messages(outputs: &[ResponseOutput]) -> Option<String> {
    if outputs.is_empty() {
        return None;
    }
    let messages = outputs
        .iter()
        .map(|output| {
            json!({
                "role": role_name(output.role),
                "parts": parts(&output.content),
                "finish_reason": output.stop_reason.map(crate::observability::stop_reason_name),
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&messages).ok()
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::System => "system",
        Role::Developer => "developer",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    }
}

fn parts(content: &[ContentBlock]) -> Vec<Value> {
    content.iter().map(part).collect()
}

fn part(block: &ContentBlock) -> Value {
    match block {
        ContentBlock::Text { text } => json!({"type": "text", "content": text}),
        ContentBlock::Reasoning { text, .. } => json!({"type": "reasoning", "content": text}),
        ContentBlock::Refusal { text } => json!({"type": "refusal", "content": text}),
        ContentBlock::ToolCall(call) => json!({
            "type": "tool_call",
            "id": call.id,
            "name": call.name,
            "arguments": call.arguments,
        }),
        ContentBlock::ToolResult(result) => json!({
            "type": "tool_call_response",
            "id": result.tool_call_id,
            "response": parts(&result.content),
        }),
        ContentBlock::Image { source } => media_part("image", image_url(source)),
        ContentBlock::Audio { source } => media_part("audio", media_url(source)),
        ContentBlock::Video { source } => media_part("video", media_url(source)),
        ContentBlock::File { source } => file_part(source),
        // The provider block is kept out: it is unnormalized provider JSON of
        // unbounded size, and its wire format is the useful part here.
        ContentBlock::Unknown { provider, .. } => {
            json!({"type": "unknown", "provider": provider.as_str()})
        }
    }
}

// Records what a media block is without its bytes; see the module docs.
fn media_part(kind: &str, url: Option<&str>) -> Value {
    match url {
        Some(url) => json!({"type": kind, "url": url}),
        None => json!({"type": kind}),
    }
}

fn file_part(source: &FileSource) -> Value {
    match source {
        FileSource::FileId(id) => json!({"type": "file", "file_id": id}),
        FileSource::FileData { filename, .. } => match filename {
            Some(filename) => json!({"type": "file", "filename": filename}),
            None => json!({"type": "file"}),
        },
        FileSource::Raw(_) => json!({"type": "file"}),
    }
}

fn image_url(source: &ImageSource) -> Option<&str> {
    match source {
        ImageSource::Url { url, .. } => Some(url.as_str()),
        ImageSource::Base64 { .. } | ImageSource::Raw(_) => None,
    }
}

fn media_url(source: &MediaSource) -> Option<&str> {
    match source {
        MediaSource::Url { url, .. } => Some(url.as_str()),
        MediaSource::Base64 { .. } | MediaSource::Raw(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use switchyard_protocol::{
        InstructionBlock, Message, StopReason, ToolCall, ToolResult, text_request,
    };

    use super::*;

    fn parse(serialized: Option<String>) -> Value {
        let serialized = serialized.expect("expected serialized content");
        serde_json::from_str(&serialized).expect("serialized content should be JSON")
    }

    /// Only `true`/`1` opt in; every other spelling leaves capture off.
    #[test]
    fn only_explicit_truth_enables_capture() {
        for value in ["true", "TRUE", " 1 "] {
            assert!(is_true(value), "{value:?} should enable capture");
        }
        for value in ["", "false", "0", "yes", "on", "no"] {
            assert!(!is_true(value), "{value:?} should not enable capture");
        }
    }

    #[test]
    fn messages_serialize_as_role_and_parts() {
        let request = text_request(Some("model".to_string()), "hello");
        assert_eq!(
            parse(input_messages(&request)),
            json!([{"role": "user", "parts": [{"type": "text", "content": "hello"}]}])
        );
        assert_eq!(input_messages(&LlmRequest::default()), None);
    }

    #[test]
    fn instructions_flatten_into_one_part_list() {
        let request = LlmRequest {
            instructions: vec![
                InstructionBlock {
                    role: Role::System,
                    content: vec![ContentBlock::Text {
                        text: "be terse".to_string(),
                    }],
                },
                InstructionBlock {
                    role: Role::Developer,
                    content: vec![ContentBlock::Text {
                        text: "cite sources".to_string(),
                    }],
                },
            ],
            ..LlmRequest::default()
        };
        assert_eq!(
            parse(system_instructions(&request)),
            json!([
                {"type": "text", "content": "be terse"},
                {"type": "text", "content": "cite sources"},
            ])
        );
        assert_eq!(system_instructions(&LlmRequest::default()), None);
    }

    /// Tool calls and their results keep the pairing id the convention uses to join them.
    #[test]
    fn tool_content_keeps_its_call_id() {
        let request = LlmRequest {
            messages: vec![
                Message {
                    role: Role::Assistant,
                    content: vec![ContentBlock::ToolCall(ToolCall {
                        id: "call_1".to_string(),
                        name: "lookup".to_string(),
                        arguments: json!({"q": "rust"}),
                    })],
                },
                Message {
                    role: Role::Tool,
                    content: vec![ContentBlock::ToolResult(ToolResult {
                        tool_call_id: "call_1".to_string(),
                        content: vec![ContentBlock::Text {
                            text: "found".to_string(),
                        }],
                        is_error: None,
                    })],
                },
            ],
            ..LlmRequest::default()
        };
        assert_eq!(
            parse(input_messages(&request)),
            json!([
                {
                    "role": "assistant",
                    "parts": [{
                        "type": "tool_call",
                        "id": "call_1",
                        "name": "lookup",
                        "arguments": {"q": "rust"},
                    }],
                },
                {
                    "role": "tool",
                    "parts": [{
                        "type": "tool_call_response",
                        "id": "call_1",
                        "response": [{"type": "text", "content": "found"}],
                    }],
                },
            ])
        );
    }

    /// Inline bytes never reach the span; a URL source keeps its URL.
    #[test]
    fn media_blocks_drop_their_payload() {
        let request = LlmRequest {
            messages: vec![Message {
                role: Role::User,
                content: vec![
                    ContentBlock::Image {
                        source: ImageSource::Base64 {
                            media_type: Some("image/png".to_string()),
                            data: "AAAABBBB".to_string(),
                        },
                    },
                    ContentBlock::Image {
                        source: ImageSource::Url {
                            url: "https://example.test/a.png".to_string(),
                            detail: None,
                        },
                    },
                ],
            }],
            ..LlmRequest::default()
        };
        let serialized = input_messages(&request).expect("expected serialized content");
        assert!(!serialized.contains("AAAABBBB"), "{serialized}");
        assert_eq!(
            parse(Some(serialized)),
            json!([{
                "role": "user",
                "parts": [
                    {"type": "image"},
                    {"type": "image", "url": "https://example.test/a.png"},
                ],
            }])
        );
    }

    #[test]
    fn outputs_carry_their_finish_reason() {
        let outputs = vec![ResponseOutput {
            role: Role::Assistant,
            content: vec![ContentBlock::Text {
                text: "answer".to_string(),
            }],
            url_citations: Vec::new(),
            stop_reason: Some(StopReason::EndTurn),
        }];
        assert_eq!(
            parse(output_messages(&outputs)),
            json!([{
                "role": "assistant",
                "parts": [{"type": "text", "content": "answer"}],
                "finish_reason": "end_turn",
            }])
        );
        assert_eq!(output_messages(&[]), None);
    }
}
