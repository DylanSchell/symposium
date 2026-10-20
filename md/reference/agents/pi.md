# Pi

Config name: `pi`

## Skills

| Scope | Path |
|-------|------|
| Project | `.agents/skills/<name>/SKILL.md` |
| Global | `~/.agents/skills/<name>/SKILL.md` |

Pi also reads `.pi/skills/` and `<agent-dir>/skills/` (its agent directory,
`~/.pi/agent` by default). Symposium uses the vendor-neutral paths it shares with
Codex, Copilot, Goose and OpenCode.

## Hooks

Pi has no hook configuration file. Its extension points are TypeScript modules,
so symposium installs one:

| Scope | File |
|-------|------|
| Project | `.pi/extensions/symposium.ts` |
| Global | `~/.pi/agent/extensions/symposium.ts` |

The extension spawns `cargo-agents hook pi <event>` for `session_start`,
`before_agent_start`, `tool_call`, `tool_result`, and `agent_end`, and applies
symposium's reply to the session. It fails open: if symposium is missing, slow, or
prints something unparseable, your tool calls and prompts proceed.

Each hook is one `cargo-agents` process: roughly 1.8 s at session start (that event
also runs a full sync) and roughly 0.5 s around each tool call inside a Rust
workspace, nearer 0.1 s elsewhere. Opening a session never waits for that sync — the
bridge collects it when you send your first message, and if it is still running by
then, on the message after.

Pi loads `.pi/extensions/` only after you grant that project trust, so global
registration is the one that takes effect immediately.

## MCP servers

| Scope | File | Key |
|-------|------|-----|
| Project | `.pi/mcp.json` | `mcpServers.<name>` |
| Global | `~/.pi/agent/mcp.json` | `mcpServers.<name>` |

The global file lives in Pi's agent directory, which `PI_CODING_AGENT_DIR`
relocates. Pi reads project MCP entries only after you grant that project trust.

## Environment

| Variable | Purpose |
|----------|---------|
| `PI_CODING_AGENT_DIR` | Relocates Pi's agent directory (default `~/.pi/agent`); symposium follows it for both files above |
| `SYMPOSIUM_PI_HOOK_BIN` | Tells the installed extension which symposium binary to run (default `cargo-agents`) |
| `SYMPOSIUM_PI_DEBUG` | When set, the extension writes hook-bridge diagnostics to stderr |
