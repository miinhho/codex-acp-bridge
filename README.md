# codex-acp-bridge

A small stdio bridge between the Agent Client Protocol (ACP) and the Codex app-server.
Codex handles authentication, model requests, tools, approvals, and session storage.

## Requirements

- Rust toolchain (edition 2024)
- Codex CLI installed and logged in (`codex login`)
- An ACP client that can start an agent process over stdio

## Build and run

```sh
cargo install --path .
codex-acp-bridge
```

Configure your ACP client to launch `codex-acp-bridge` as its stdio agent command. The
binary starts `codex app-server --stdio` for that connection and exits when the client
closes the connection.

To select a model for newly created sessions, set `CODEX_ACP_MODEL` in the agent
process environment. For example:

```sh
CODEX_ACP_MODEL=gpt-6-luna codex-acp-bridge
```

If unset, Codex uses the model from its own configuration. The selected model must be
available to the installed Codex account. Resumed sessions keep the model stored with
that Codex thread.

## Supported ACP behavior

- Initialize and authenticate (authentication is handled by the local Codex CLI)
- Create, list, and resume sessions
- Text, image, embedded text resource, and embedded image blob prompts
- Streamed assistant text and image data URL history replay
- Command and file-change permission requests
- Turn cancellation
- Command and file tool updates

Audio and non-image embedded blobs are not supported. History replay restores images
stored as base64 data URLs. Other image references are shown as text because the bridge
does not fetch arbitrary URLs or paths from stored sessions. ACP model selection is not
exposed; use `CODEX_ACP_MODEL` to choose a model for newly created sessions.

## Checks

```sh
cargo test --offline
cargo clippy --offline --all-targets -- -D warnings
cargo fmt --check
```

The end-to-end tests use a local fake Codex app-server and generated JSON cases. They do
not call a model or require a Codex login.
