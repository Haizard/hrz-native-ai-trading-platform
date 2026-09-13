# Frontend

Not yet scaffolded. This directory is reserved for the layout fixed in
`docs/03-PROJECT-STRUCTURE.md`:

```
frontend/
├── app/            # Leptos/Dioxus application shell (or React+TS fallback)
└── chart-engine/   # Rust/WASM chart renderer (separate wasm crate)
```

## Open decision gate (docs/14-FRONTEND-CHART-ENGINE.md)

**Status: UNDECIDED** — must be resolved and dated here before Phase 7 starts.

The target architecture is Rust-first (Leptos/Dioxus + WASM + Canvas/WebGPU) so the
chart engine shares `analytics-core` directly with the backend, and so footprint /
volume-profile rendering doesn't bottleneck on per-cell DOM elements.

The accepted pragmatic fallback is a **React+TypeScript application shell** hosting the
**chart engine as a Rust/WASM module** via a canvas element, if agent velocity in Rust
web frameworks proves materially slower.

What is non-negotiable either way:

> There must never be a second implementation of the trading math in TypeScript.
> The chart engine, whichever shell hosts it, calls into the same Rust
> `analytics-core` compiled to WASM.

Fill in when decided:

- **Decision:**
- **Date:**
- **Rationale:**
