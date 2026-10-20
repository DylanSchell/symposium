# Pi Hooks Reference

> **Disclaimer:** This document reflects our current understanding of Pi's extensibility surface.
> It is a working reference for symposium development, not a substitute for the official docs.
> Details may be outdated or incomplete — always consult the primary sources.
>
> **Primary sources:**
> [Extensions](https://pi.dev/docs/latest/extensions)
> · [Skills](https://pi.dev/docs/latest/skills)
> · [MCP servers](https://pi.dev/docs/latest/mcp)
> · [Configuration](https://pi.dev/docs/latest/configuration)
> · [GitHub repo](https://github.com/earendil-works/pi)

**Pi has no shell-command hooks.** There is no `hooks.json`, no JSON stdin/stdout
protocol, and no exit-code convention. Its interception points are TypeScript
modules loaded into the Pi process, which register handlers with
`pi.on(event, handler)`.

Symposium therefore ships the other half of the protocol itself: a small
TypeScript extension that listens for Pi's events, spawns
`cargo-agents hook pi <event>`, and applies the JSON on stdout to the running
session. Installing that file **is** Pi's hook registration.

## Hook registration

| Scope | File |
|---|---|
| Global | `<agent-dir>/extensions/symposium.ts` (`~/.pi/agent/extensions/`) |
| Project | `.pi/extensions/symposium.ts` |

The extension source lives at `src/agents/assets/pi-extension.ts` and is embedded
in the binary with `include_str!`, so an installed file always matches the binary
that wrote it and a sync needs no network.

Pi reads every file in its extensions directory, so there is nothing to merge with
a user's own hooks: symposium owns exactly one file, identified by its first line,
`// symposium:managed`.

| Situation | Behavior |
|---|---|
| File absent | Written (directory created) |
| File present, managed, identical | Untouched — registration reports "already registered" and the mtime is preserved |
| File present, managed, different | Overwritten, so an older symposium upgrades itself |
| File present, **not** managed | Left alone with a warning; symposium hooks are not installed |
| Unregistration | Deletes the managed file, leaves every other extension alone |

Two caveats:

- **Project scope needs trust.** Pi reads `.pi/extensions/` only after the user
  grants [project trust](https://pi.dev/docs/latest/security), so a project-scoped
  install is inert until then. Global scope works immediately.
- **Extensions run in-process with full OS permissions.** That is Pi's documented
  model for extensions, and the reason the bridge is a file symposium owns and
  visibly rewrites rather than something downloaded at run time.

## Event mapping

| Pi event | Symposium event | What symposium does with it |
|---|---|---|
| `session_start` | session-start | **Starts** the hook without awaiting it (see below), then **holds** the context: `session_start` is notification-only, so nothing it returns reaches the conversation. |
| `before_agent_start` | user-prompt-submit | Runs the hook and returns held plus fresh context as one custom message. Dispatched only when `messageType === "user"`. |
| `tool_call` | pre-tool-use | Returns `{ block, reason }` to deny; mutates `event.input` in place to rewrite arguments. |
| `tool_result` | post-tool-use | Appends symposium's context to `content`, passing `structuredContent` back so Pi does not drop it. |
| `agent_end` | stop | Notification only; its output is discarded, as on every other agent. |

Injection, in other words: Pi's session-start event cannot inject, so the context
waits for the first run, where `before_agent_start` can add a message. That makes
session-start and user-prompt-submit share one delivery point, and `messageType`
is what keeps a `pi.sendMessage(…, { triggerTurn: true })` run — which is not a
user prompt — from dispatching one (`AgentHookInput::should_dispatch`).

Pi fires `session_start` on `/reload` too, which is a resource reload rather than a
new conversation. The extension ignores that reason, or the same context would be
injected twice into one session.

Pi **awaits** `session_start` handlers, so nothing may `await` inside one.
Symposium's SessionStart runs a full auto-sync; a handler that blocks there delays
the session itself (measured: a handler that sleeps 5 s makes Pi take 5 s longer to
open). So the extension starts the hook and parks the promise, and the first
`before_agent_start` collects it - waiting no longer than a prompt hook would have
run. A stalled sync then delivers its context on the next run rather than holding
the user's first message for the hook's 60 s timeout.

## Wire format

Input on stdin is one JSON object, shaped like the canonical format so plugin
authors meet few surprises:

```json
{
  "hook_event_name": "PreToolUse",
  "tool_name": "bash",
  "tool_input": { "command": "cargo test" },
  "session_id": "01J…",
  "cwd": "/work/project"
}
```

`PostToolUse` adds `tool_input` and `tool_response` (`{ content, isError }`), and
`UserPromptSubmit` adds `prompt` and `message_type`.

Output on stdout carries only what Pi can act on:

```json
{ "block": true, "reason": "denied by the decisions plugin" }
```

```json
{ "updatedInput": { "command": "cargo nextest run" } }
```

```json
{ "additionalContext": "…" }
```

- `block` / `reason` — the denial. Pi reads no exit code from an extension handler.
- `updatedInput` — applied by mutating `event.input`. Pi has no `ask` state, so a
  rewrite is applied directly, the way Antigravity's `overwrite` is; Pi's own
  permission handling then sees the rewritten call.
- `additionalContext` — the only thing the non-tool events return.
- Anything else, including `{}`, means "nothing to apply".

Because symposium defines both halves, a plugin can also ask for this format
directly (`[[hooks]] format = "pi"`), and it fires only when the host is Pi.

## Failure handling

Every path is fail-open: an unspawnable binary, a timeout, or output that is not
JSON all resolve to "nothing to say", and the tool call or prompt proceeds. A hook
that guides the agent must never be able to stop one.

| Knob | Value |
|---|---|
| `SYMPOSIUM_PI_HOOK_BIN` | Path to the symposium binary (default `cargo-agents`) |
| `SYMPOSIUM_PI_DEBUG` | Set to any value to write bridge diagnostics to stderr |
| Tool-call / prompt timeout | 10 s |
| Session-start timeout | 60 s — session-start also refreshes plugin sources, which may reach the network |

A per-tool-call hook means one process spawn per tool call, before and after. Cost,
measured end to end through `hook pi <event>` on one machine with a warm cache -
order of magnitude, not a budget:

| Event | Cost | Why |
|---|---|---|
| session-start | ~1.8 s | always a full auto-sync plus `prewarm_hook_sources`; network-bound, so treat this as the floor |
| pre-tool-use / post-tool-use, in a Rust workspace | ~0.5 s each | auto-sync itself skips here (`Cargo.lock` unchanged) |
| the same, outside a workspace | ~0.1 s | sync bails out early |

The ~0.5 s is not process startup (~10 ms) and not workspace discovery (~30 ms).
Most of it is one `crate_sources::probe` per matched plugin that carries a crate
requirement, running `cargo fetch` plus `cargo metadata` on every invocation.
That belongs to the shared hook path - `hook claude` pays the same - so the fix is
a follow-up there (resolve hooks before touching cargo, and return early when the
agent has no registered hooks) rather than anything Pi-specific.

This is why the tool-call timeout is short and the failure path is silent.

## Skill registration

Pi reads both the vendor-neutral Agent Skills locations and its own resource
directories, scanning directories containing `SKILL.md` recursively:

| Scope | Paths Pi reads | Symposium installs to |
|---|---|---|
| Project | `.agents/skills/`, `.pi/skills/` | `.agents/skills/<name>/SKILL.md` |
| Global | `~/.agents/skills/`, `<agent-dir>/skills/` | `~/.agents/skills/<name>/SKILL.md` |

The vendor-neutral path is the one symposium writes, so one directory serves Pi
alongside Codex, Copilot, Goose and OpenCode. A project `.agents/skills/` is
discovered from the working directory upward, stopping at the repository root.

`<agent-dir>` is `~/.pi/agent` unless `PI_CODING_AGENT_DIR` names another
directory. Symposium does not write skills into it: `~/.agents/skills/` needs no
knowledge of that variable. Its two Pi-owned files — `extensions/symposium.ts` and
`mcp.json` — do follow the variable.

## MCP server registration

```json
{
  "mcpServers": {
    "symposium": {
      "command": "/path/to/cargo-agents",
      "args": ["mcp"]
    }
  }
}
```

- **User-level**: `<agent-dir>/mcp.json` — `~/.pi/agent/mcp.json`, or
  `$PI_CODING_AGENT_DIR/mcp.json` when that variable is set.
- **Project-level**: `.pi/mcp.json`. Pi reads it only after the user grants
  [project trust](https://pi.dev/docs/latest/security), and a project entry
  replaces a user-level entry of the same name.

The entry shape is the common one: `mcpServers.<name>` with `command`, `args`, and
an `env` **map**, plus `url`/`headers` for a remote server. Pi documents its format
as "matches other MCP clients", and `pi mcp add` writes exactly this shape, so
symposium's shared serializer applies unchanged.

Both scopes were verified with the CLI rather than read off the docs:
`pi mcp add symposium -- cargo-agents mcp` reported writing
`<agent-dir>/mcp.json` (honoring `PI_CODING_AGENT_DIR`), and the same command with
`--local` reported writing `<project>/.pi/mcp.json`. `pi mcp list` is the check for
an entry Pi actually accepts.

## Custom instructions

| File | Scope |
|---|---|
| `AGENTS.md`, `CLAUDE.md` (working directory and its parents) | Project |
| `AGENTS.override.md` | Either, replaces the plain context file in the same directory |
| `<agent-dir>/AGENTS.md`, `<agent-dir>/CLAUDE.md` | Global |

Context-file discovery does not require project trust.
