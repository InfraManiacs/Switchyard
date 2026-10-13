# About this fork

`InfraManiacs/Switchyard` — a fork of [NVIDIA-NeMo/Switchyard](https://github.com/NVIDIA-NeMo/Switchyard),
Apache-2.0, carrying OpenTelemetry GenAI patches used by
[kube_ai_data_platform](https://github.com/InfraManiacs/kube_ai_data_platform).

**The goal is for this file to get shorter.** Everything here is generic and
upstreamable; nothing is deployment-specific. Patches held in a fork are patches
someone rebases forever.

## What it adds

Upstream routes a call and reports token counts, but does not put the message
content or the provider-reported cost onto the span. A platform whose thesis is
that trace exhaust is the product needs both.

| Commit | Adds |
|---|---|
| `Update Otel and Image Build Process` | `content_capture.rs` — writes `gen_ai.input.messages`, `gen_ai.output.messages`, `gen_ai.system_instructions` onto per-call spans, gated on `OTEL_INSTRUMENTATION_GENAI_CAPTURE_MESSAGE_CONTENT`. Resolves `gen_ai.provider.name` from the backend host. Adds `switchyard.route` and `switchyard.target.name` span fields, so a trace carries the route the caller asked for and the target alias that served it — labels that survive a model swap. Adds the multi-arch GHCR publish workflow. |
| `Fix Token Cost maths to be real` | Takes cost from what the provider actually reported (`usage.cost`) instead of reconstructing it from a rate table. Against an aggregator, reconstruction is structurally wrong, not merely imprecise: one model id is dispatched to whichever upstream is cheapest that second, each with its own price. |
| `Fix Telemetry` | Codec-level fixes across the OpenAI chat, Anthropic and Responses codecs, buffered and streaming, so cost and usage survive every path. |
| `Improve Telemetry Segmentation` | Separates the gateway and router hops so their spans are distinguishable. |

Four integration tests cover it: `gen_ai_content_capture.rs`, plus
`client_call_span_records_provider_reported_cost`,
`client_call_span_names_the_configured_target` and
`streamed_usage_updates_the_client_call_span` in `tests/observability.rs`.

## Rebasing onto a newer upstream

This fork was last rebased onto upstream `main` at `f26dafe4`, 203 commits past
its original divergence point. Notes for whoever does it next:

- **`run.rs` is where the work is.** Upstream is active in this file. The fork
  threads two extra arguments — `route: Option<&str>` and
  `routing_overhead: Option<Duration>` — through `serve` → `call_first_available`
  → `call_one`. Both are additive span labels; re-apply the *intent*, not the diff.
- **Do not reintroduce `CallPhase`.** The fork originally carried a `CallPhase`
  enum to distinguish routing calls from the terminal completion. Upstream now
  draws that distinction itself with the `buffer` flag on `call_one` and a
  separate inline routing path in `serve`. Keeping both would be two sources of
  truth for one fact.
- **`ClientRouting` must not require `Clone`.** It owns a `Mutex` of provider
  state. `with_target_names` mutates through `Arc::get_mut` and panics if the
  router is already shared, which is a configuration-time invariant.
- **Test fixtures drift.** Upstream regularly adds fields to `HttpBackendConfig`,
  `Response` and `ResponseOutput`. Prefer upstream's own fixture helpers over the
  fork's hand-built literals wherever they exist.

Verify with: `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D warnings`,
`cargo fmt --all --check`.

## Versioning

Tags are `vX.Y.Z-otel.N`, cut one patch above the upstream release this fork sits
ahead of — `v0.3.1-otel.1` for a fork ahead of upstream `v0.3.0`. A `v0.3.0-otel.N`
tag would sort *before* `v0.3.0` under semver prerelease rules and misrepresent
the base. If upstream cuts a real `v0.3.1`, the next fork tag moves to
`v0.3.2-otel.1`.

Images publish to `ghcr.io/inframaniacs/switchyard-server`, multi-arch
(linux/amd64 + linux/arm64). **Deploy an immutable tag, never `main`** — a
floating tag can put two replicas on two different builds at once, which presents
as intermittent telemetry loss rather than as a stale image.
