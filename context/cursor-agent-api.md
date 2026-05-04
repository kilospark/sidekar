# Cursor Agent API — Research Notes

**Date:** 2026-04-29 (REPL subsection updated 2026-05-03)

**REPL evolution:** A native Rust REPL stub now exists (`src/providers/cursor.rs`; default backend `https://api2.cursor.sh`). Full streaming is **not** implemented — see **`context/cursor-repl-rust-attempts.md`** for abandoned approaches (Cloud Agents API, Node/`@cursor/sdk` bridge, guessing protos) and the remaining checklist.

Below, the original **AiService / StreamChat** protobuf field tables remain useful baseline research; captures may also show **`agent.v1.AgentService`** (`Run`, `RunSSE`, …) from newer bundles.

## Current State

- **PTY mode**: Cursor/agent is fully supported via `CursorFamily` in `src/agent_cli/cursor_family.rs`. PTY wraps the external `cursor`/`agent` binary with startup injection, argv enrichment, broker registration, bus poller, relay tunnel.
- **REPL mode**: `Provider::Cursor` — exchange + optional Connect unary probe only; **`stream`** blocked on committed protobuf + bidirectional **`Run`** (or explicitly chosen RPC) from captures. Not a substitute for PTY until that lands.

## Research Findings

### Auth
- Credentials stored in **macOS Keychain**: account `cursor-user`, services `cursor-access-token` / `cursor-refresh-token` / `cursor-api-key`.
- Fallback: file-based `~/.cursor-agent/auth.json` (not observed on this machine).
- Request headers: `Authorization: Bearer <access_token>`, `x-request-id: <uuid>`, `x-cursor-client-version: cli-<version>`, `x-cursor-client-type: cli`, `x-ghost-mode: false`.

### API Protocol
- **Base URL:** `https://api2.cursor.sh`
- **Protocol:** ConnectRPC (HTTP/1.1, `@connectrpc/connect-node`)
- **Service:** `aiserver.v1.AiService`
- **Serialization:** Protobuf only for streaming endpoints. JSON works for unary (e.g., `AvailableModels`).

### Key RPC Methods
| Method | Type | Notes |
|---|---|---|
| `AvailableModels` | Unary | Returns model list. Works with JSON. |
| `StreamChat` | Server-streaming | Main chat endpoint. Protobuf only (`application/connect+proto`). |
| `StreamChatToolformer` | Server-streaming | Tool-use variant. |
| `StreamChatToolformerContinue` | Server-streaming | Tool-use continuation. |
| `StreamChatTryReallyHard` | Server-streaming | Retry variant. |

### Protobuf Schema (extracted from minified JS)

**GetChatRequest** (input to StreamChat):
| Field | No | Kind | Type |
|---|---|---|---|
| current_file | 1 | message | CurrentFile |
| conversation | 2 | message (repeated) | ConversationMessage |
| repositories | 3 | message (repeated) | Repository |
| explicit_context | 4 | message | ExplicitContext |
| workspace_root_path | 5 | string (opt) | |
| code_blocks | 6 | message (repeated) | CodeBlock |
| model_details | 7 | message | ModelDetails |
| request_id | 9 | string | |
| conversation_id | 15 | string | |
| desired_max_tokens | 26 | int32 (opt) | |
| should_cache | 29 | bool (opt) | |
| allow_model_fallbacks | 30 | bool (opt) | |

**ConversationMessage**:
| Field | No | Kind |
|---|---|---|
| text | 1 | string |
| type | 2 | enum MessageType |

**MessageType enum**: UNSPECIFIED=0, HUMAN=1, AI=2

**ModelDetails**: field 1 = model_name (string)

**StreamChatResponse**: field 2 = text (string)

### Why full REPL chat is risky / paused (historic “why skip this” list)
Same blockers apply to **shipping** Cursor as a first-class Rust chat backend; **`cursor.rs`** only implements **exchange + unary probe** until protos are nailed from captures.
1. **No public API contract** — schema extracted from minified JS bundle, will break silently on updates.
2. **Protobuf-only streaming** — requires Connect envelope framing + correct serialization for proprietary messages. No JSON fallback for streaming.
3. **No official third-party HTTP contract** — behavior is inferred from tooling.
4. **PTY mode works well** — wrapping the `cursor`/`agent` CLI remains the dependable integration path.
5. **Token lifecycle** — refresh/expiry semantics for IDE vs API key flows are easy to misunderstand.
6. **Maintenance burden** — a full protobuf client tied to unpublished wire shapes is ongoing toil for unclear payoff vs PTY.

---

# OpenCode Go Plan — Research & Implementation

**Date:** 2026-04-29
**Decision:** Added as REPL credential provider. Prefix: `ocg-`. Provider type: `opencode-go`.

## Architecture

OpenCode has two API plans that share the same API key (`OPENCODE_API_KEY`):

| | OpenCode Zen | OpenCode Go |
|---|---|---|
| Base URL | `https://opencode.ai/zen/v1` | `https://opencode.ai/zen/go/v1` |
| API shape | OpenAI-compatible (sidekar routes as Anthropic — both accepted) | Same |
| Auth | `Authorization: Bearer <api_key>` | Same key |
| Models | Premium (Claude, GPT, Gemini, etc.) | Budget open-weight (Kimi K2.5/K2.6, MiniMax M2.5/M2.7, GLM-5/5.1, DeepSeek V4, Qwen 3.5+/3.6+, Mimo V2/V2.5) |
| Credential prefix | `oc-` / `opencode-` | `ocg-` |
| Provider type | `opencode` | `opencode-go` |
| KV key | `oauth:opencode` | `oauth:opencode-go` |
| Model list endpoint | `/zen/v1/models` (no auth required) | `/zen/go/v1/models` (no auth required) |

## Source

- GitHub: `https://github.com/anomalyco/opencode` (TypeScript monorepo)
- TUI: `packages/opencode/src/provider/provider.ts`
- Go plan defined in `models.dev` API responses, not hardcoded in TUI
- Uses `@ai-sdk/openai-compatible` SDK

## Files Changed

- `src/providers/oauth.rs` — `provider_type_for()`, `stored_provider_type_for()`, `KV_KEY_OPENCODE_GO`, `get_opencode_go_token()`, `login_opencode_go()`
- `src/providers/mod.rs` — `Provider::opencode_go()`, `provider_type()` (zen/go detection), `fetch_opencode_go_model_list()`, `fetch_model_list()` dispatch
- `src/repl/slash.rs` — `build_provider()` arm for `"opencode-go"`
- `src/main/repl_cmd.rs` — login/credentials/models dispatch for `ocg` prefix
