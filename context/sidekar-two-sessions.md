# Two meanings of “session” in Sidekar

Sidekar overloads the word **session**. Keeping these separate avoids confusion when debugging bus commands vs browser automation.

## 1. Agent bus (PTY + broker)

**What it is:** Local coordination between agent processes: registration, nicknames, typed messages (`bus send`, `bus done`), and `bus who` listings. State lives in the SQLite broker (for example under `~/.sidekar/`). Delivery uses Unix domain sockets and the broker’s queue.

**How you get on the bus:** Run an agent through the PTY wrapper (`sidekar claude`, `sidekar codex`, etc.). The wrapper registers the child with a **channel** and identity; the child process can participate in bus messaging. Environment such as `SIDEKAR_PTY` indicates the PTY-owned path.

**This is not Chrome.** No browser needs to be running for bus registration itself.

## 2. Chrome / CDP session (browser automation)

**What it is:** A **browser automation** context: DevTools Protocol connection to Chrome, CDP port, `state-{session_id}.json`, tab/window state, and commands like `navigate`, `screenshot`, `click`.

**How you get it:** `sidekar launch` or `sidekar connect` (or equivalent). The app tracks a **session id** in `AppContext` (`current_session_id`) and discovers it via files like the per-agent **last-session** pointer and `state-*.json`.

**This is not the bus.** It is the CDP side of the tool.

## Why the CLI blurs the words

Many commands run through **`auto_discover_last_session()`**, which reads the **last Chrome session** pointer from disk. So errors like **“No active session. Run: sidekar launch”** refer to **missing Chrome/CDP session** in that code path, not to “nobody registered on the agent bus.”

Bus-related commands (`bus who`, `bus send`, `bus done`) are conceptually about the **broker**, but they may still require that session discovery step depending on how the binary dispatches commands, so a failure can look like a bus problem when it is actually **no discovered browser session**.

## Mental model

| Concept | Role | Typical entry |
|--------|------|------------------|
| **Agent bus** | Multi-agent coordination, messages | PTY wrapper (`sidekar <agent>`) |
| **Chrome session** | Browser control via CDP | `sidekar launch` / `sidekar connect` |

When in doubt: **bus = agents and broker; session = Chrome/CDP automation state.**

For the four **user-facing capability pillars** (browser, desktop, inter-agent, background), see `context/feature-pillars.md`.
