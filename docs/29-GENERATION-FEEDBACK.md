# 29 — Generation feedback: what the chat shows while the model works

## Purpose

Indicator generation is a 15–60 second synchronous request (model draft →
vet → repair loop → preview). Before this document the studio chat showed a
static "Generating…" in the status line and nothing else — no sense of
liveness, no elapsed time, no way out but closing the tab.

## What the user sees now

- **A pending bubble in the transcript itself** — an AI bubble with a dashed,
  slowly pulsing border that reads "Generating — the model drafts, the vet
  checks and repairs, usually 15–60s · **23s**". It lives in the chat stream,
  so it scrolls with the conversation and is wiped by the transcript
  re-render when the answer lands.
- **A live elapsed counter**, one tick per second.
- **The send button becomes the stop button** (➤ → ■, Enter works too).
  Clicking it aborts the *wait* via an `AbortController`. The label is
  honest: the server may still finish the generation, and the answer appears
  on the next refresh of that workspace.
- **Switching chats or leaving the conversation stops waiting** — the bubble
  belongs to the chat that was asked.

## The honesty rule

The bubble describes the pipeline once and counts seconds. It never shows a
per-stage progress bar, because the stages are not observable: the request is
one synchronous POST and no stage information crosses the wire until the
response arrives. A fake stage animation would be a lie about what the
system knows. Real per-stage progress needs a streaming transport (SSE) on
`create_message` — that retrofit is the full roadmap #1 and stays open; this
document is its honest interim.

## What this is not

- **Not cancellation.** Aborting stops the client from waiting; the gateway
  does not yet take a cancel signal mid-generation.
- **Not attempt telemetry.** The model-attempt count still arrives with the
  response (and is reported in the answer), not mid-flight.
