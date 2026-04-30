# Cursor Agent API — Research Notes

**Date:** 2026-04-29
**Decision:** Do NOT add Cursor as a REPL credential provider. The API is proprietary ConnectRPC/protobuf with no public contract — reverse-engineering it is fragile and not worth the effort.

## Current State

- **PTY mode**: Cursor/agent is fully supported via `CursorFamily` in `src/agent_cli/cursor_family.rs`. PTY wraps the external `cursor`/`agent` binary with startup injection, argv enrichment, broker registration, bus poller, relay tunnel.
- **REPL mode**: No Cursor provider. Not planned.

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

### Why Not Worth It
1. **No public API contract** — schema extracted from minified JS bundle, will break silently on updates.
2. **Protobuf-only streaming** — requires implementing ConnectRPC envelope framing + protobuf serialization for a proprietary schema. No JSON fallback for streaming.
3. **No official SDK or documentation** — Cursor does not publish an API for third-party use.
4. **PTY mode already works** — wrapping the `cursor`/`agent` CLI binary is the supported path.
5. **Token lifecycle unclear** — refresh token rotation, expiry, and re-auth flows are opaque.
6. **Effort/value mismatch** — maintaining a ConnectRPC protobuf client against an undocumented, frequently-changing API is ongoing toil for marginal benefit over PTY mode.

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
