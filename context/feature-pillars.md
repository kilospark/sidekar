# Sidekar feature pillars

User-facing capabilities are grouped into **four pillars**. Other docs sometimes mention “web research,” “batch runs,” or “MCP.” Those are either **use cases** on top of the pillars or **implementation / distribution** details, not separate product pillars.

## 1. Browser automation (CDP + Chrome extension)

- **CDP:** Full Chrome DevTools Protocol over WebSocket: navigation, interaction, perception, screenshots, network, PDFs, etc.
- **Extension:** The MV3 extension under `extension/` complements CDP where an in-page bridge is needed (same automation story, not a fifth pillar).

## 2. Desktop automation

- **macOS:** Native UI via Accessibility API: apps, windows, elements, screenshots, input. Use it for dialogs and native surfaces outside CDP-driven pages.

## 3. Inter-agent communication and orchestration

- **Bus:** Local registry and messaging (SQLite broker, Unix sockets): discovery, `bus send`, `bus done`, handoffs, durable queues.
- **Orchestration** is the user outcome; PTY helpers (`sidekar claude`, etc.) are one way to run multi-agent workflows, not a separate pillar.

## 4. Background automation

- **Monitor:** Tab title/favicon watching, debounced, notifications via the bus.
- **Cron:** Scheduled tool execution with persistence for reactive and unattended work.

## Related

- **Two meanings of “session”** (Chrome/CDP vs bus): see `context/sidekar-two-sessions.md`.
