# Rust ACP ↔ Codex Bridge

## Intent

공식 `codex-acp`와 같은 역할을 하는 **매우 가벼운 Rust bridge**를 만든다.

현재 공식 `codex-acp`는 Node/TypeScript 기반이라 단순히 Codex를 ACP로 호출하기 위한 용도에 비해 runtime overhead가 크다.

다행히 양쪽 모두 공식 Rust 구현을 활용할 수 있다.

* ACP: `agent-client-protocol` 공식 Rust SDK
* Codex: OpenAI Codex의 `codex-app-server-protocol`

따라서 새로운 Agent 구현을 만드는 것이 아니라, **두 공식 protocol 사이를 연결하는 thin adapter**를 만드는 것이 목적이다.

```text
ACP Client
    │
    │ ACP / stdio
    ▼
Rust Bridge
    │
    │ Codex App Server Protocol
    ▼
codex app-server
```

## Scope

Bridge의 책임은 가능한 한 작게 유지한다.

주요 역할은:

* ACP stdio endpoint 제공
* `codex app-server` lifecycle 관리
* ACP request를 Codex app-server request로 변환
* Codex event를 ACP update로 변환
* session/thread ID 등 translation에 필요한 최소 상태 관리

예상되는 주요 mapping은 다음과 같다.

```text
ACP                         Codex

initialize              ↔   initialize
session/new             ↔   thread/start
session/prompt          ↔   turn/start
session/cancel          ↔   turn/interrupt
session/update          ←   Codex notifications
permission              ↔   approval
```

정확한 mapping은 현재 upstream protocol과 공식 `codex-acp` 구현을 확인해서 결정한다.

## Design constraints

이 프로젝트는 **Agent framework/library가 아니다.**

`Session`, `Prompt`, `ContentBlock`, `ToolCall`, permission 등 ACP에 이미 존재하는 abstraction은 그대로 사용해도 된다.

반면 다음과 같은 자체적인 범용 abstraction은 만들지 않는다.

```text
Agent
AgentBackend
UniversalSession
UniversalEvent
AgentRuntime
```

Codex의 auth, tools, sandbox, MCP, persistence 등의 책임도 가져오지 않는다. 이는 계속 `codex app-server`의 책임이다.

HTTP server나 persistent daemon도 필요 없다. 기본 형태는 ACP client가 필요할 때 process를 실행하고 connection이 끝나면 종료되는 stdio application이다.

## Runtime

Tokio의 single-thread runtime을 우선 사용한다.

```rust
#[tokio::main(flavor = "current_thread")]
```

이 프로그램은 대부분 stdio/network I/O를 기다리는 bridge이므로 처음부터 multi-thread runtime을 사용할 이유는 없다.

Shared state와 복잡한 concurrency도 피한다. 필요한 최소한의 async task/channel만 사용한다.

## Dependencies

가능하면 다음 두 공식 구현에 의존한다.

```text
agent-client-protocol
codex-app-server-protocol
```

ACP JSON-RPC나 Codex protocol type을 자체적으로 다시 정의하지 않는다.

Codex dependency는 `main`을 무조건 따라가기보다 known-good commit을 pin하는 방향을 선호한다.

## Implementation approach

먼저 다음 upstream source를 확인한다.

1. 공식 ACP Rust SDK에서 stdio Agent를 가장 작게 구현하는 방법
2. `codex-app-server-protocol`의 현재 request/notification types
3. 공식 Node `codex-acp`의 ACP ↔ Codex mapping

공식 Node `codex-acp`는 **behavior reference**로 사용하되 구조를 그대로 포팅할 필요는 없다.

첫 구현은 `initialize → session/new → session/prompt → streaming → completion`이 동작하는 최소 vertical slice부터 시작한다.

그 이후 cancel, approval/tool events 등 실제 ACP 호환성에 필요한 기능을 추가한다.

전체 구현에서 가장 중요한 원칙은:

> ACP와 Codex가 이미 제공하는 protocol abstraction을 재사용하고, 그 사이에 새로운 Agent abstraction을 만들지 않는다.

최종 결과물은 가능한 한 boring하고 작은 protocol bridge여야 한다.
