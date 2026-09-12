// SPDX-FileCopyrightText: Copyright (c) 2026 NVIDIA CORPORATION & AFFILIATES. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Process-wide tracing and OpenTelemetry setup for server hosts.
//!
//! Export is driven by the standard OTLP environment variables read here
//! (`OTEL_EXPORTER_OTLP_ENDPOINT`, `OTEL_SERVICE_NAME`, `OTEL_SDK_DISABLED`, ...).
//! One further variable affects what traces contain rather than where they go:
//! `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`, which is off unless set to
//! `true` or `1` and, when set, puts the full prompt and completion of every model
//! call into the exported spans. That is a deliberate data-handling choice, not a
//! verbosity level — see `switchyard-server`'s README and the `content_capture`
//! module of `switchyard-llm-client`, which reads it, for what it exposes.

use std::env;
use std::sync::OnceLock;

use axum::http::HeaderMap;
use opentelemetry::propagation::{Extractor, TextMapPropagator};
use opentelemetry::trace::TracerProvider as _;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::propagation::TraceContextPropagator;
use opentelemetry_sdk::trace::SdkTracerProvider;
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::layer::SubscriberExt as _;
use tracing_subscriber::util::SubscriberInitExt as _;
use tracing_subscriber::{EnvFilter, Layer as _};

use crate::{ServerError, ServerResult, metrics};

const DEFAULT_LOG_FILTER: &str = "info,opentelemetry=warn";
const DEFAULT_SERVICE_NAME: &str = "switchyard-server";

struct Observability {
    tracer_provider: Option<SdkTracerProvider>,
}

static OBSERVABILITY: OnceLock<Result<Observability, String>> = OnceLock::new();

/// Installs metrics and tracing once for either the binary or an embedded host.
pub fn initialize_observability() -> ServerResult<()> {
    match OBSERVABILITY.get_or_init(initialize) {
        Ok(_) => Ok(()),
        Err(error) => Err(ServerError::new(error.clone())),
    }
}

/// Flushes pending OTLP telemetry without shutting down process-wide providers.
pub fn flush_observability() {
    if let Some(Ok(observability)) = OBSERVABILITY.get()
        && let Some(provider) = &observability.tracer_provider
        && let Err(error) = provider.force_flush()
    {
        tracing::warn!(error = %error, "failed to flush OpenTelemetry traces");
    }
    metrics::flush();
}

/// Header carrying the authenticated caller's identity, as written by the API
/// gateway in front of this server.
///
/// Envoy Gateway's `SecurityPolicy.apiKeyAuth` sets this via its
/// `forwardClientIDHeader` option: after it validates the caller's API key it
/// adds the matched *client id* (the Secret's data-key name, e.g. `jsims`) as
/// this header's value, and with `sanitize` enabled it strips the API key
/// itself before the request reaches us. The value is therefore an identity
/// the gateway has already authenticated, not caller-supplied input - a client
/// cannot set it themselves, because the gateway overwrites it.
const CLIENT_ID_HEADER: &str = "x-client-id";

/// Creates the server request span with any incoming W3C trace context as its parent.
///
/// When the gateway forwarded a caller identity, it is recorded as `user.id`.
/// That is the attribute Langfuse maps onto its own user dimension (it reads
/// `user.id` as well as `langfuse.user.id`), so recording it here is what makes
/// per-caller filtering and cost attribution possible downstream. Absent the
/// header the field stays unset rather than being filled with a placeholder,
/// so "no identity" is distinguishable from a real one.
pub(crate) fn request_span(headers: &HeaderMap) -> tracing::Span {
    let parent = TraceContextPropagator::new().extract(&HeaderExtractor(headers));
    let span = tracing::info_span!(
        target: "switchyard_server",
        "switchyard.request",
        otel.kind = "server",
        openinference.span.kind = "CHAIN",
        user.id = tracing::field::Empty,
    );
    if let Some(client_id) = headers
        .get(CLIENT_ID_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        span.record("user.id", client_id);
    }
    let _ = span.set_parent(parent);
    span
}

struct HeaderExtractor<'a>(&'a HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key).and_then(|value| value.to_str().ok())
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(|name| name.as_str()).collect()
    }
}

pub(crate) fn otlp_enabled(signal: &str) -> bool {
    if env_var_is_true("OTEL_SDK_DISABLED") {
        return false;
    }
    if env::var(format!("OTEL_{signal}_EXPORTER"))
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some_and(|value| {
            !value
                .split(',')
                .any(|exporter| exporter.trim().eq_ignore_ascii_case("otlp"))
        })
    {
        return false;
    }
    [
        "OTEL_EXPORTER_OTLP_ENDPOINT",
        &format!("OTEL_EXPORTER_OTLP_{signal}_ENDPOINT"),
    ]
    .into_iter()
    .any(|name| env::var(name).is_ok_and(|value| !value.trim().is_empty()))
}

pub(crate) fn resource() -> Resource {
    let service_name = env::var("OTEL_SERVICE_NAME")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SERVICE_NAME.to_string());
    Resource::builder().with_service_name(service_name).build()
}

fn initialize() -> Result<Observability, String> {
    metrics::registry()?;

    let tracer_provider = otlp_enabled("TRACES")
        .then(build_tracer_provider)
        .transpose()?;
    let filter = log_filter()?;
    let format = tracing_subscriber::fmt::layer()
        .with_ansi(false)
        .with_writer(std::io::stderr)
        .with_filter(filter);

    if let Some(provider) = &tracer_provider {
        let tracer = provider.tracer("switchyard");
        tracing_subscriber::registry()
            .with(format)
            .with(
                tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(log_filter()?),
            )
            .try_init()
            .map_err(|error| format!("failed to initialize tracing: {error}"))?;
    } else {
        tracing_subscriber::registry()
            .with(format)
            .try_init()
            .map_err(|error| format!("failed to initialize tracing: {error}"))?;
    }

    Ok(Observability { tracer_provider })
}

fn log_filter() -> Result<EnvFilter, String> {
    EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new(DEFAULT_LOG_FILTER))
        .map_err(|error| format!("invalid tracing filter: {error}"))
}

fn build_tracer_provider() -> Result<SdkTracerProvider, String> {
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .build()
        .map_err(|error| format!("failed to initialize OTLP trace exporter: {error}"))?;
    let provider = SdkTracerProvider::builder()
        .with_resource(resource())
        .with_batch_exporter(exporter)
        .build();
    opentelemetry::global::set_tracer_provider(provider.clone());
    Ok(provider)
}

fn env_var_is_true(name: &str) -> bool {
    env::var(name).is_ok_and(|value| matches!(value.to_ascii_lowercase().as_str(), "true" | "1"))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    use axum::http::{HeaderMap, HeaderValue};
    use opentelemetry::trace::{TraceContextExt, TracerProvider as _};
    use opentelemetry_sdk::trace::SdkTracerProvider;
    use tracing::Subscriber;
    use tracing::field::{Field, Visit};
    use tracing::span::{Attributes, Id, Record};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::{Context as LayerContext, SubscriberExt as _};
    use tracing_subscriber::registry::LookupSpan;

    use super::request_span;

    /// Collects span fields so a test can assert on what was recorded, covering
    /// both creation-time fields and later `record` calls.
    #[derive(Clone, Default)]
    struct Captured(Arc<Mutex<BTreeMap<String, String>>>);

    struct StringVisitor<'a>(&'a mut BTreeMap<String, String>);

    impl Visit for StringVisitor<'_> {
        fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
            self.0
                .insert(field.name().to_string(), format!("{value:?}"));
        }

        fn record_str(&mut self, field: &Field, value: &str) {
            self.0.insert(field.name().to_string(), value.to_string());
        }
    }

    impl<S> Layer<S> for Captured
    where
        S: Subscriber + for<'a> LookupSpan<'a>,
    {
        fn on_new_span(&self, attrs: &Attributes<'_>, _id: &Id, _ctx: LayerContext<'_, S>) {
            attrs.record(&mut StringVisitor(&mut self.0.lock().unwrap()));
        }

        fn on_record(&self, _id: &Id, values: &Record<'_>, _ctx: LayerContext<'_, S>) {
            values.record(&mut StringVisitor(&mut self.0.lock().unwrap()));
        }
    }

    #[test]
    fn request_span_records_forwarded_client_identity() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());
        let mut headers = HeaderMap::new();
        headers.insert("x-client-id", HeaderValue::from_static("jsims"));

        tracing::subscriber::with_default(subscriber, || {
            let _span = request_span(&headers);
        });

        assert_eq!(
            captured
                .0
                .lock()
                .unwrap()
                .get("user.id")
                .map(String::as_str),
            Some("jsims"),
            "the gateway-forwarded client id must land on the span as user.id, \
             which is what Langfuse maps to its user dimension"
        );
    }

    #[test]
    fn request_span_leaves_user_id_unset_without_client_header() {
        let captured = Captured::default();
        let subscriber = tracing_subscriber::registry().with(captured.clone());

        tracing::subscriber::with_default(subscriber, || {
            let _span = request_span(&HeaderMap::new());
        });

        // An unfilled Empty field must stay absent rather than recording a
        // placeholder, so unattributed traffic is distinguishable from a real
        // caller literally named "unknown" or "".
        assert_eq!(captured.0.lock().unwrap().get("user.id"), None);
    }

    #[test]
    fn request_span_continues_incoming_w3c_trace_context() {
        let provider = SdkTracerProvider::builder().build();
        let tracer = provider.tracer("request-span-test");
        let subscriber =
            tracing_subscriber::registry().with(tracing_opentelemetry::layer().with_tracer(tracer));
        let mut headers = HeaderMap::new();
        headers.insert(
            "traceparent",
            HeaderValue::from_static("00-4bf92f3577b34da6a3ce929d0e0e4736-00f067aa0ba902b7-01"),
        );
        headers.insert(
            "tracestate",
            HeaderValue::from_static("vendor=opaque-value"),
        );

        tracing::subscriber::with_default(subscriber, || {
            let span = request_span(&headers);
            let context = span.context();
            let current = context.span();
            let span_context = current.span_context();
            assert_eq!(
                span_context.trace_id().to_string(),
                "4bf92f3577b34da6a3ce929d0e0e4736"
            );
            assert_eq!(span_context.trace_state().header(), "vendor=opaque-value");
        });
    }
}
