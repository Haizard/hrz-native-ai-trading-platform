# 36 — Streamed generation: the pipeline's work, live

## Purpose

#1-full. A workspace generation used to be one opaque 15–60s wait with an
elapsed timer. Now the pending bubble narrates the **real** pipeline as it
happens: `drafting · attempt 2/5`, `repairing · attempt 1 — plot(close: line
12: expected )`, `autofix · appended sec="ETHUSDT"`, `previewing on the
stored candles`. Not token streaming, deliberately — the model produces a
`submit_script` tool call, not prose, so there are no answer tokens to
stream; what streams is the work, the same precedent as the agent socket
(`ws.rs`: "What the socket streams instead is the work"). And no fabricated
stages: every frame is a thing the pipeline actually did.

## Wire

`POST /indicator-workspaces/{id}/messages/stream` — SSE:

- `event: progress` — a `GenerationEvent` (`{stage: "drafting", attempt,
  max}` / `{stage: "repairing", attempt, errors}` / `{stage: "autofix",
  detail}` / `{stage: "previewing"}`), emitted from inside the
  validate–repair loop and the preview step.
- `event: done` — **exactly** the payload the plain route returns, so the
  client handles both transports identically.
- `event: error` — `{status, body}` with the plain route's own error body,
  because once streaming starts the HTTP status is 200 and the event is the
  error channel. Rejections before streaming (malformed JSON, auth) remain
  ordinary HTTP errors — which is how the shell tells "stream" from
  "fall back".

Keep-alive `: ping` comments ride every 15s so a proxy cannot mistake the
long LLM wait for a dead connection.

## The pipeline is not changed by being observed

`create_message` and `create_message_stream` share one body,
`run_workspace_turn`; the plain route passes `progress: None`. A pipeline
that behaved differently when observed would be a bug. A disconnected client
closes the channel and the send fails silently — generation continues so its
result still persists (the send button's "stop waiting" text has always said
exactly that).

## The shell

SSE over `fetch` with a `ReadableStream` parser, because `EventSource`
cannot POST. Progress frames land in the pending bubble's stage line; `done`
flows through the same handler as before. A gateway without the route
answers 404 before any SSE begins, and the call falls back to the plain
POST — the client works against old and new gateways alike.

## Tests

- `pine_codegen::tests::the_progress_stream_reports_each_attempt_and_repair`
  — a scripted LLM fails the vet once then passes; the frames are
  `drafting → repairing(carrying the vet's own words) → drafting`, in order.
- `pine_codegen::tests::no_listener_means_no_events_and_no_change` — the
  `None` sink keeps the pipeline byte-identical to its pre-stream behavior.

## What this is not

Not provider token streaming: `LlmClient::complete` is a one-shot shape, and
teaching providers to stream partial tool calls is an ai-agent change of its
own, for a payload (a tool call, not prose) where partial tokens carry
little value.
