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

- reqwest, default features off, with `rustls-no-provider` and `http2`.
- rustls with the ring provider, installed when the client is created.
- The system's certificate verifier, plus a configurable CA bundle.
- One client per process, created lazily on a background task, with a connection opened ahead of the first request.

**Amended in step 5, on 2026-11-12, when the transport was built.**

- **`stream` and `json` are not turned on.** This record first listed them. The response body is read chunk by chunk with the client's own `chunk()`, and requests are serialised by the protocols, so neither feature is used.
- **Redirects are not followed.** A model endpoint has no reason to redirect, and following one would carry the credential headers to wherever it points. A redirect is returned as the response it is.
- **The transport has no time limits except for making a connection** (30 s). The limits on a response, for its first event and for silence after it, belong to the client above the transport, where they run on the same clock as the retry layer and are tested without waiting ([architecture/04-providers.md](../architecture/04-providers.md), "Transport").
- **Measured again** with the feature set as built (reqwest 0.13, rustls 0.23 with ring, tokio), by the same method on the same machine: 6.4 s clean in the dev profile and 13.2 s in release, 3,035 KB added, 92 packages. Creating the client: 1.06 to 1.13 ms warm over ten runs, and 44.6 ms on the first run after a build. Making the transport, which is all that happens at start-up, takes under a microsecond.

## Why

- ring over aws-lc-rs: measurably faster to build and smaller, and no extra build tools on Windows. The rustls maintainers have committed to security maintenance of ring.
- reqwest over hyper directly: 1.8 s of dev build and 0.65 MB buy proxies, redirects, timeouts and pooling that are easy to get subtly wrong. The transport sits behind our own `Transport` trait, so this can change without touching the protocols.

## Revisit when

- ring loses its security maintenance, or a provider requires an algorithm it lacks.
- The binary-size or CI budget is under pressure. Moving to hyper directly is the first saving to take.
