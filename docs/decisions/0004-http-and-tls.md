# 0004: reqwest with rustls and the ring provider

- **Status:** Accepted
- **Date:** 2026-11-12

## Context

The provider client needs HTTPS with streaming response bodies, HTTP/2, proxies, timeouts and connection pooling, on Linux and Windows, and it is the largest dependency the core cannot avoid. It affects binary size, clean build time on both CI runners, and start-up.

## Options considered and measured

Clean builds pinned to 4 cores on the desktop, to approximate a hosted CI runner. Sizes are the stripped release binary of a minimal program that uses the client, minus an empty program. Full method and tables: [measurements/dependency-costs.md](../measurements/dependency-costs.md).

| Option | Dev build | Release build | Size added | Crates | Notes |
|---|---|---|---|---|---|
| reqwest, rustls, **aws-lc-rs** (reqwest's default) | 13.7 s | 25.9 s | 5.1 MB | 105 | aws-lc-rs compiles a large C library. On Windows it needs NASM or prebuilt objects. |
| reqwest, rustls, **ring** | 8.6 s | 15.9 s | 3.1 MB | 101 | ring needs only a C compiler. |
| hyper + hyper-util + hyper-rustls, ring | 6.8 s | 13.4 s | 2.5 MB | 56 | We would write redirects, proxy handling, timeouts and pooling configuration ourselves. |
| ureq (blocking) | not chosen | | | | A blocking client needs a thread per stream and hand-built cancellation. |
| native TLS | not measured | | | | OpenSSL on Linux is a system dependency or a slow vendored build. It also gives different TLS behaviour per OS. |

In the full dependency set, choosing aws-lc-rs over ring costs 4.6 s of clean dev build, 2.9 s of release build and 1.9 MB of binary.

**Start-up.** Building the HTTP client takes 1.2 to 1.5 ms warm and 45 ms on a cold file cache, because it loads the system's certificate store. It must not be on the start-up path.

## Decision

- reqwest, default features off, with `rustls-no-provider`, `http2`, `stream` and `json`.
- rustls with the ring provider, installed at start.
- The system's certificate verifier, plus a configurable CA bundle.
- One client per process, created lazily on a background task, with a connection opened ahead of the first request.

## Why

- ring over aws-lc-rs: measurably faster to build and smaller, and no extra build tools on Windows. The rustls maintainers have committed to security maintenance of ring.
- reqwest over hyper directly: 1.8 s of dev build and 0.65 MB buy proxies, redirects, timeouts and pooling that are easy to get subtly wrong. The transport sits behind our own `Transport` trait, so this can change without touching the protocols.

## Revisit when

- ring loses its security maintenance, or a provider requires an algorithm it lacks.
- The binary-size or CI budget is under pressure. Moving to hyper directly is the first saving to take.
