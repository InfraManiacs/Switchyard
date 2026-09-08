// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Opt-in GenAI content capture on the per-call span.
//!
//! The opt-in is read once per process, so this lives in its own integration binary:
//! `cargo test` runs each `tests/*.rs` file as a separate process, which is the only
//! way to exercise the enabled path without disturbing the default-off assertions in
//! `tests/observability.rs`.

use std::sync::Arc;

use http::HeaderMap;

use async_trait::async_trait;
use futures::StreamExt;
use opentelemetry::Value as OtelValue;
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use serde_json::{Value, json};
use switchyard_libsy::{Algorithm, Driver, LibsyError, RoutingOutcome, RuntimeModels};
use switchyard_llm_client::ClientRouter;
use switchyard_protocol::{
    ContentBlock, InstructionBlock, LlmClientError, LlmRequest, LlmResponse, LlmResponseChunk,
    LlmResponseStreamEvent, Message, Request, Response, ResponseOutput, Role, RoutedLlmClient,
    StopReason, Usage,
};
use tracing_subscriber::layer::SubscriberExt as _;

const ALGO: &str = "content-capture-algo";
const AGG_MODEL: &str = "content-capture-agg-model";
const STREAM_MODEL: &str = "content-capture-stream-model";
const PROMPT: &str = "summarize the incident report";
const INSTRUCTION: &str = "answer in one sentence";
const COMPLETION: &str = "the disk filled up";

/// Answers with a buffered response, or a streamed one for the streaming target.
struct EchoClient;

#[async_trait]
impl RoutedLlmClient for EchoClient {
    async fn call(&self, request: Request) -> Result<Response, LlmClientError> {
        let model = request.model_id().map(|model| model.to_string());
        if model.as_deref() == Some(STREAM_MODEL) {
            let events = vec![Ok(LlmResponseStreamEvent::new(vec![
                LlmResponseChunk::MessageStart {
                    id: Some("content-capture-stream".to_string()),
                    model,
                },
                LlmResponseChunk::TextDelta {
                    index: 0,
                    text: COMPLETION.to_string(),
                },
                LlmResponseChunk::Usage(Usage::default()),
                LlmResponseChunk::MessageStop {
                    reason: Some("end_turn".to_string()),
                },
            ]))];
            return Ok(Response {
                llm_response: LlmResponse::Stream(Box::pin(futures::stream::iter(events))),
                metadata: None,
                upstream_headers: HeaderMap::new(),
            });
        }
        Ok(Response {
            llm_response: LlmResponse::Agg(switchyard_protocol::AggLlmResponse {
                model,
                outputs: vec![ResponseOutput {
                    role: Role::Assistant,
                    content: vec![ContentBlock::Text {
                        text: COMPLETION.to_string(),
                    }],
                    url_citations: Vec::new(),
                    stop_reason: Some(StopReason::EndTurn),
                }],
                ..switchyard_protocol::AggLlmResponse::default()
            }),
            metadata: None,
            upstream_headers: HeaderMap::new(),
        })
    }
}

/// Routes straight to one target and leaves the terminal call to the client.
struct SingleCallAlgo(String);

#[async_trait]
impl Algorithm for SingleCallAlgo {
    fn name(&self) -> &str {
        ALGO
    }

    async fn route(
        self: Arc<Self>,
        _driver: Driver,
        request: Request,
    ) -> Result<RoutingOutcome, LibsyError> {
        Ok(RoutingOutcome::route_to(
            self.0.clone().into(),
            Vec::new(),
            request,
        ))
    }
}

fn request(model: &str) -> Request {
    Request {
        llm_request: LlmRequest {
            model: Some(model.to_string()),
            instructions: vec![InstructionBlock {
                role: Role::System,
                content: vec![ContentBlock::Text {
                    text: INSTRUCTION.to_string(),
                }],
            }],
            messages: vec![Message::text(Role::User, PROMPT)],
            ..LlmRequest::default()
        },
        raw_request: None,
        metadata: None,
    }
}

fn find_span(exporter: &InMemorySpanExporter, model: &str) -> SpanData {
    let spans = match exporter.get_finished_spans() {
        Ok(spans) => spans,
        Err(error) => panic!("failed to read exported spans: {error}"),
    };
    match spans
        .iter()
        .find(|span| span.name == format!("chat {model}"))
    {
        Some(span) => span.clone(),
        None => panic!("no exported client-call span for {model}"),
    }
}

fn attribute(span: &SpanData, key: &str) -> Value {
    let value = span
        .attributes
        .iter()
        .find(|attribute| attribute.key.as_str() == key)
        .map(|attribute| attribute.value.clone());
    let Some(OtelValue::String(value)) = value else {
        panic!("{key} is missing or not a string on {:?}", span.name);
    };
    serde_json::from_str(value.as_str()).expect("captured content should be JSON")
}

/// Both response shapes capture prompt, instructions, and completion once the
/// operator opts in. The buffered path has the whole response up front; the streamed
/// path only knows its content after the last event, so it records on stream end.
#[tokio::test]
async fn opting_in_captures_request_and_response_content() -> Result<(), LibsyError> {
    // Set before any span exists, and before the runtime has anything else to run:
    // the opt-in is read once, on the first observed call.
    unsafe {
        std::env::set_var("OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT", "true");
    }
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("content-capture-test")));
    let _default = tracing::subscriber::set_default(subscriber);

    for model in [AGG_MODEL, STREAM_MODEL] {
        let (_, response) = switchyard_llm_client::run(
            Arc::new(SingleCallAlgo(model.to_string())),
            ClientRouter::single(Arc::new(EchoClient)),
            request(model),
            Arc::new(RuntimeModels::default()),
            None,
        )
        .await?;
        if let LlmResponse::Stream(mut stream) = response.llm_response {
            while stream.next().await.is_some() {}
        }
    }

    let expected_input = json!([{"role": "user", "parts": [{"type": "text", "content": PROMPT}]}]);
    let expected_instructions = json!([{"type": "text", "content": INSTRUCTION}]);
    let expected_output = json!([{
        "role": "assistant",
        "parts": [{"type": "text", "content": COMPLETION}],
        "finish_reason": "end_turn",
    }]);
    for model in [AGG_MODEL, STREAM_MODEL] {
        let span = find_span(&exporter, model);
        assert_eq!(attribute(&span, "gen_ai.input.messages"), expected_input);
        assert_eq!(
            attribute(&span, "gen_ai.system_instructions"),
            expected_instructions
        );
        assert_eq!(attribute(&span, "gen_ai.output.messages"), expected_output);
    }
    Ok(())
}
