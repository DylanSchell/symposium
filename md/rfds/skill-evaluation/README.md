# Evaluating agent tasks with and without Symposium

## TL;DR

- Add an internal `cargo xtask agent-eval` runner that runs real Claude Code and Codex CLI sessions against small Rust tasks, with and without Symposium, and grades the result with code.
- Start with three tasks: a **capability** task that compares the two configurations on a made-up crate the model cannot already know, a **regression** task that checks skills and hooks are delivered on first use, and a **negative** task that checks a skill stays out of a project it does not apply to.
- Measure Symposium as a chain: was the skill **installed**, could the client **discover** it, was it **observed loaded**, did the outcome **improve**, and at what **cost**.
- Enforce isolation by running each client, and each grading step, inside [`srt`](https://github.com/anthropics/sandbox-runtime), which restricts reads, writes, and network for the whole process tree. Without `srt`, results are labeled exploratory and never compared.
- Grade a trusted copy of the project that takes only the files the task allows the agent to edit, count timeouts as failures, and report an observation that cannot be interpreted as inconclusive rather than as a pass or a fail.



## Motivation

We want to measure whether Symposium changes how an agent performs a task. Three gaps prevent that today.

First, our agent-mode fixtures always initialize *and* synchronize Symposium before the agent starts (`with_fixture` in `symposium-testlib/src/lib.rs` runs `init` then `sync`). Skills are already installed when the session begins, so no test exercises the path a new user hits: the first `SessionStart` hook installing skills on its own. That path has at least one known gap: Claude Code discovers skills before `SessionStart` hooks finish, so a skill a hook installs is missing from the first prompt unless the hook asks for a rescan ([Claude Code hooks](https://code.claude.com/docs/en/hooks)), and Symposium does not ask today.

Second, there is no baseline. Every agent-mode run has Symposium present, so we cannot compare an ordinary agent run against one with Symposium on the same task.

Third, delivering a skill is not the same as the agent using it. In [Vercel's evaluation](https://vercel.com/blog/agents-md-outperforms-skills-in-our-agent-evals), an agent never invoked an available skill in 56% of cases, and with the skill available but unprompted, results matched having no documentation at all. Symposium's value depends on every step from installation to use, so each step needs its own measurement.

This RFD builds the smallest runner that closes these gaps. It follows practices that recur across published guidance on agent evaluation (see [the FAQ](#what-did-we-take-from-existing-evaluation-practice)): isolated trials, outcome-based graders, several trials per configuration, balanced positive and negative cases, and reference solutions.

## Change in a nutshell



### Vocabulary

We use the terms from [Anthropic's guide to agent evals](https://anthropic.com/engineering/demystifying-evals-for-ai-agents):

- A **task** is one test: a starter project, a prompt, and success criteria.
- A **trial** is one attempt at a task in one configuration (with or without Symposium).
- A **grader** checks one aspect of the result. All graders here are code.
- The **transcript** is the client's full event log for a trial.
- The **outcome** is the final state of the project, not what the agent says it did.



### The capability task

The main comparison uses a fixture crate, `fixture-store`, that exists only in this repository. Its `Store::put` buffers writes in memory, and they reach disk only on `Store::flush`. A `Store` dropped without flushing loses its writes silently. The code compiles and runs either way. The crate cannot have been memorized, but a baseline may still infer the rule from familiar buffering conventions or from the task's persistence requirement; pilot runs will show whether the task discriminates.

The task's starter is a small library that depends on `fixture-store` by path. The prompt:

> Add a function `save_settings(dir: &Path, settings: &[(&str, &str)]) -> std::io::Result<()>` to src/lib.rs that saves each key/value pair with fixture-store, using a `Store` opened at `dir`.

A registry plugin, gated on the dependency, carries a skill that states the rule:

```toml
name = "fixture-store"
depends-on = ["fixture-store"]

[[skills]]
source.path = "skills"
```

```markdown
---
name: fixture-store
description: Use when reading or writing data with the fixture-store crate.
---

`Store::put` buffers writes in memory. They reach disk only when `Store::flush`
is called, and a `Store` dropped without flushing loses them without an error.
Call `flush()` before returning from any function that writes.
```

After the trial, the runner grades the agent's `src/` against the original crate, with a held-out test the agent never saw. It calls `save_settings`, opens a *new* `Store` at the same directory, and checks every value is there. Without the flush, that test fails.

The runner compares the two configurations over several trials:


|                                    | Without Symposium | With Symposium                   |
| ---------------------------------- | ----------------- | -------------------------------- |
| Skill installed / discoverable     | (none)            | by the first `SessionStart` hook |
| Skill observed loaded              | (none)            | from the transcript              |
| Task succeeded (of started trials) | 2 of 5            | 5 of 5                           |
| Mean tokens, time                  | baseline          | difference reported              |


(Numbers are illustrative.) A diligent baseline can still discover the rule by reading the crate's source or by testing a reopened store. That is a fair result: the success-rate difference shrinks, and the cost difference shows what the skill saved.

### The other two tasks


| Task                   | Kind       | Question it answers                                                                 |
| ---------------------- | ---------- | ----------------------------------------------------------------------------------- |
| `store-durability`     | capability | Does the delivered skill improve the outcome?                                       |
| `formatting`           | regression | Are skills and plugin hooks delivered on first use? Expected to succeed every time. |
| `store-not-applicable` | negative   | Does a skill stay out of a project that does not depend on its crate?               |


The `formatting` task asks for a small `double` function in a crate with no dependencies. Its plugin (`depends-on = ["*"]`) carries a formatting skill and a `Stop` hook that records `cargo fmt --check` and then runs `cargo fmt`. It checks the whole delivery path with a real client: `SessionStart` installs the skill, the client can discover it, and the `Stop` hook fires.

The `store-not-applicable` task registers the `fixture-store` plugin but gives the agent a starter that does not depend on `fixture-store`. The skill must not be installed, and no load of it may be observed. Absence alone would also be the result if Symposium never ran, so the task registers a second plugin as a positive control: `eval-control`, with `depends-on = ["*"]` and one skill that must be installed. The control skill being present shows that synchronization ran against the configured registry, which is what makes the missing `fixture-store` skill evidence of dependency filtering.

Regression and negative tasks answer questions about Symposium alone, so they run only with Symposium by default. Capability tasks always run both configurations.

See [the proposed usage guide](./usage-guide.md) for the author-facing flow.

## Detailed plans



### Scope

The first implementation supports the three tasks above, two clients (Claude Code and Codex CLI), sequential trials, code graders, and aggregated results. Each comparison uses the same client version, model and model settings, prompt, starter, toolchain, isolation settings, and time limits for both configurations.

Out of scope: LLM or human graders, loading `evals.json` files, more clients, parallel trials, and dependency changes during a session. No public `cargo agents` subcommand is added.

### Task format

Plugins, fixture crates, and tasks live in separate directories, so they can be shared:

```text
evals/
  plugins/
    fixture-store/          # Symposium.toml, skills/fixture-store/SKILL.md
    format-on-stop/         # Symposium.toml, skills/, scripts/format-on-stop.sh
    eval-control/           # depends-on = ["*"], one skill; positive control for negative tasks
  fixtures/
    fixture-store/          # the made-up crate
  tasks/
    store-durability/
      task.toml
      starter/              # library depending on ../fixtures/fixture-store
      checks/               # held-out tests, one file per check
      solution/             # reference solution, copied over the starter
      naive/                # plausible solution without the skill's knowledge
    formatting/
    store-not-applicable/
```

`task.toml` uses the field names of the [Agent Skills](https://agentskills.io/skill-creation/evaluating-skills) `evals.json` where they overlap (`prompt`, `expected_output`), so that format can be imported later:

```toml
kind = "capability"            # capability | regression | negative
plugins = ["fixture-store"]
fixtures = ["fixture-store"]
editable = ["src/**"]          # the only files graded; the default
fmt_check = false
prompt = """
Add a function `save_settings(dir: &Path, settings: &[(&str, &str)]) -> std::io::Result<()>` \
to src/lib.rs that saves each key/value pair with fixture-store, using a `Store` opened at `dir`.
"""
expected_output = "save_settings writes every pair so that a newly opened Store sees it."
naive_fails = ["settings_persist"]   # held-out checks the naive solution must fail

[skills]
delivered = ["fixture-store"]  # must be installed and discoverable with Symposium
not_delivered = []             # must be neither installed nor observed loaded

[hooks]
expected = []                  # plugin hooks that must leave a record
```

Fixture crates are copied into the trial outside the project (`<run>/fixtures/`) and are read-only to the agent. They are not workspace members and have no `skills/` directory, so the only way their rule reaches the agent through Symposium is the registry plugin.

### Graders

All graders are code and run after the agent and every hook have finished. They grade the outcome, not the steps the agent took.

Grading never runs in the agent's project. The runner first preserves the submission as-is, then builds a **trusted grading project**: a fresh copy of the starter and fixtures from `evals/`, with only the `editable` files taken from the submission. Edits anywhere else, such as to `Cargo.toml`, a build script, `.cargo/config.toml`, or the fixture crate, are listed in the result but never graded, so they cannot change what the tests mean.

Within the `editable` paths, the submission replaces the starter exactly: the starter's editable files are removed first, then the submission's are copied in, so a file the agent deleted stays deleted. Only regular files and directories are accepted. A symlink or special file under an editable path makes the submission invalid, which counts as a failed trial, and every destination is checked to resolve inside the grading project before it is written.

Every grader runs under a deadline. What happens when something goes wrong depends on whose fault it is:

- **Submission failure**: the agent's code does not compile, a test fails or crashes, or a test exceeds its deadline (for example, a `save_settings` that loops forever). The submission is what failed, so the trial is a counted failure.
- **Grading infrastructure error**: the sandbox could not launch, a trusted fixture was unavailable, or the runner could not collect results. The evaluation failed, not the agent, so a completed trial is excluded and reported, and its kept submission can be graded again. A trial that already failed by timeout or client error stays a counted failure.
- **Task graders**: `cargo test --lib` passes, however many tests the agent wrote; each held-out test passes, run as an explicit target (`cargo test --test <name>`); `cargo fmt --check` passes when the task sets `fmt_check`. Held-out targets are runner-owned, so each must report the same number of tests run as when `--check-tasks` ran it, which catches tests that were disabled or never discovered. If a task requires the agent to write a test, that requirement gets its own held-out check rather than a count.
- **Delivery graders** (Symposium configuration): each `delivered` skill is *installed* (present with its `.symposium` marker, with content matching the plugin it came from) and *discoverable* (see below); each `not_delivered` skill is not installed.
- **Use graders** (Symposium configuration): each skill's *observed load*, from the transcript: `loaded` (a successful `Skill` tool call naming it, or a successful read of its installed `SKILL.md`), `attempted` (such a call that failed or was denied), `not_observed`, or `unknown` when the adapter cannot interpret the transcript. This is evidence of exposure, not proof the agent followed the skill, and a client could supply a skill in a way the adapter does not see. Each adapter documents what it detects.
- **Integration graders** (Symposium configuration): the skill directory was empty before launch; a `SessionStart` hook trace exists with a successful exit; synchronization completed, shown by the task's positive-control or `delivered` skills being installed and by Symposium recording a successful sync for the project (it records one only after a sync succeeds); each expected plugin hook left at least one record. A missing `SessionStart` invocation fails integration; evidence too incomplete to tell whether synchronization completed makes it inconclusive. `Stop` can fire more than once, so a hook's first record is the one graded.

Every grader result is `pass`, `fail`, or `inconclusive`. Inconclusive means the evidence cannot decide either way, and it is never silently turned into a pass or a fail.

**Discoverable** is the hardest of these to observe, because no client is known to report its available skills reliably. It is decided as follows:

- `pass` when the agent successfully loaded the skill (an observed `loaded`), or when the client's transcript lists the skill among those available. A successful load proves the skill was accessible and takes precedence over any listing.
- `fail` when the transcript holds a complete listing of available skills, captured after any rediscovery, that omits this one, and no successful load was observed.
- `inconclusive` otherwise. The absence of a listing proves nothing.

Step 4 records which fields of each client's transcript, if any, list available skills. The `formatting` regression task names its skill in the prompt ("Use the rust-formatting skill"), so a working delivery path is expected to produce an observed load and therefore a decided result. That instruction is acceptable there because the task measures delivery, not whether the agent chooses a skill on its own; capability tasks never name a skill.

#### What the evidence can and cannot show

Hook records, hook traces, and installed skills are written by processes inside the trial: Symposium's hooks, which the client starts. The same boundary that lets them write lets the agent write there too, so the agent could, in principle, alter a record, edit an installed skill, or change the hook configuration. Isolation from the host does not make these records independent.

The RFD therefore treats them as supporting evidence, not tamper-proof proof, and cross-checks what it can from outside the trial:

- installed skill contents are compared with the plugin in `evals/`, which the trial cannot read or write;
- the client's hook and Symposium configuration are snapshotted after `init` and compared after the trial, and any change is recorded;
- the transcript, which the runner captures from the client's output stream rather than from a file, is the primary record of what the agent did, including any write to `evidence/` or `home/`.

A hook record contradicted by the transcript, or a changed configuration, makes the affected graders inconclusive. Independently verified hook execution would need a recording channel outside the agent's writable paths; that is follow-up work if tampering is ever observed.

### Success by task kind

A trial can succeed only if its execution completed normally. A timeout or client error is a counted failure even when the code it left behind would pass grading; that code is still graded and reported, as diagnostic evidence. The failure stands whatever the diagnostic grading does: a grading infrastructure error during it is recorded, but never moves the trial out of the denominator. Grading exclusions apply only to a completed trial, whose outcome grading is needed to decide. For a completed trial, success depends on what its task is for:


| Kind       | Succeeds when                                                                                                                                                                          | Inconclusive when                                                                                            |
| ---------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------ |
| Capability | Every task grader passes. Delivery and use are reported alongside, never filtered out: a skill that was not delivered is part of Symposium's end-to-end result.                        | Never: task graders always decide.                                                                           |
| Regression | Every task, delivery, and integration grader passes.                                                                                                                                   | Any of those is inconclusive and none failed.                                                                |
| Negative   | Every integration grader passes, including the positive control, every `not_delivered` skill is absent, and the adapter interpreted the whole transcript with no load of one observed. | An integration grader is inconclusive, or the transcript could not be fully interpreted, and nothing failed. |




### Reference solutions

Every task has a `solution/` that must pass all task graders. A capability task also has a `naive/` solution, written as an agent without the skill plausibly would. It must compile, pass `cargo test --lib`, and fail exactly the held-out checks listed in `naive_fails`. Together they show the task can be solved and that the held-out tests catch the specific mistake the skill prevents, not just any mistake.

`cargo xtask agent-eval --check-tasks` runs both through the trusted grading project and records each held-out target's test count for later trials. The reference and naive solutions are code from this repository, not from an agent, so this check runs without `srt`, client, model, or API key, and runs in CI. A 0% success rate is most often a broken task or grader, and this check rules that out before any model is called.

### Trials and aggregation

`--trials N` runs each configuration N times. Each trial starts from a fresh run directory, and the order of configurations alternates between trials (baseline first, then Symposium first, and so on), so neither configuration always benefits from running second. The default is 1, which suits regression and negative tasks; a comparison uses 5 or more.

Each planned trial ends in exactly one of these places:


| Count                           | Meaning                                                                                   |
| ------------------------------- | ----------------------------------------------------------------------------------------- |
| setup failures                  | the trial never launched its client                                                       |
| cancelled                       | the batch stopped before the trial ran                                                    |
| isolation exclusions            | the client launched, but the trial violated isolation                                     |
| grading exclusions              | the trial completed, but grading needed to decide its outcome hit an infrastructure error |
| succeeded, failed, inconclusive | everything else, by outcome                                                               |


The totals follow from those:

```text
planned  = setup failures + cancelled + started
eligible = started - isolation exclusions - grading exclusions
         = succeeded + failed + inconclusive
```

For each task and configuration, the runner writes a `benchmark.json`:

- **success rate**: succeeded divided by eligible. Timeouts and client errors stay in the denominator, since an unreliable configuration should not look better by failing early. For regression and negative tasks, the counts of succeeded, failed, and inconclusive are reported alongside, since an inconclusive trial is neither.
- **setup failures, cancellations, and exclusions**, by cause. Any count above zero is a reason to inspect the batch before reading the rates.
- **pass_all**: whether every eligible trial succeeded, or `null` when there are none.
- **observed load counts**: `loaded`, `attempted`, `not_observed`, and `unknown`, per skill.
- **cost**: mean tokens (with input, output, and cache components) and mean agent time. Each mean carries its own count of contributing trials, since a client may report time but not tokens. Agent time is the client session only, excluding setup and grading. Cost is reported over all eligible trials and over succeeded trials separately, so a configuration that is cheaper only because it fails quickly does not look like a saving.
- **delta**: the Symposium configuration minus the baseline, for each rate and mean.

A rate with no eligible trials is `null`, never 0 or 1. Five trials per configuration provide limited evidence; the runner reports numbers and does not claim significance.

### What "without Symposium" means

The `cargo-agents` binary stays installed on the developer's machine throughout. "Without Symposium" means no process in the trial can reach it, not that it is absent from the machine. Nothing is installed or uninstalled between configurations.

Symposium is an input to the runner, not part of any trial. `--symposium-bin <path>` names the binary to evaluate; without it, the runner builds `cargo-agents` from the current checkout once per batch. Pointing it at an installed release evaluates that release instead, with no other change.


|                                     | Without Symposium | With Symposium                    |
| ----------------------------------- | ----------------- | --------------------------------- |
| `<run>/bin/cargo-agents`            | absent            | link to the staged binary         |
| Staged binary in `tools/symposium/` | unreadable        | readable                          |
| Hooks in `<run>/home`               | none              | registered by `cargo agents init` |
| Developer's installed copies        | unreadable        | unreadable                        |


Symposium registers its hooks as `cargo-agents hook <agent> <event>`, a bare name the client resolves on `PATH`. The trial's `PATH` holds no developer directories, and the `srt` boundary makes the developer's home unreadable, so a copy installed there cannot be reached by any path, relative, absolute, or through a script.

### Preparing a trial

All batches live under one evaluation root, and each batch has a read-only `tools/` directory plus one directory per trial:

```text
<eval-root>/<batch>/
  tools/
    common/                      # staged clients, readable by every trial
    symposium/                   # staged cargo-agents, readable by Symposium trials only
  trial-1-with-symposium/        # <run>
    home/         # HOME for every process in the trial
    project/      # copy of the starter
    fixtures/     # read-only copies of the task's fixture crates
    bin/          # links to staged tools: the client, sh, and (Symposium only) cargo-agents
    cargo-home/   # CARGO_HOME, empty at start
    tmp/          # TMPDIR
    evidence/     # EVAL_EVIDENCE_DIR and SYMPOSIUM_HOOK_TRACE, outside the project
    registry/     # the task's plugins (Symposium configuration only)
```

The executables a trial runs are **staged** once per batch: the client into `<batch>/tools/common/` (a single binary for native installs; for npm installs, the package directory and the Node.js runtime), and the `cargo-agents` binary under evaluation into `<batch>/tools/symposium/`. They are copied, not linked. Only Symposium trials can read `tools/symposium/`; for a baseline trial it stays inside the denied evaluation root, so the staged binary cannot be run even by absolute path. On Linux, `srt` matches read rules by the name a path is opened under, and a link inside an allowed directory does not reopen a denied target, so a link from `bin/` to a client installed under the developer's home would not launch. Staging also fixes the binaries for the whole batch, and their hashes go into each result.

Claude Code (`~/.claude`), Codex (`~/.codex`), and Symposium (`~/.symposium`) all keep their configuration, hooks, and state under the home directory, so pointing `HOME` (and `USERPROFILE` on Windows) at `<run>/home` gives each trial fresh client and Symposium state.

The client starts with a cleared environment holding only `HOME`, `PATH`, `TMPDIR`, `CARGO_HOME`, `EVAL_EVIDENCE_DIR`, `SYMPOSIUM_HOOK_TRACE` (the file Symposium's hook trace writer appends to), and the model API key. `PATH` is, in order:

1. `<run>/bin`: links to the staged client in `tools/common/`, plus `cargo-agents` from `tools/symposium/` in the Symposium configuration, and to the system `sh`.
2. The active toolchain's own `bin/` directory inside `RUSTUP_HOME` (the directory of `rustup which cargo`), which holds only Rust tools.
3. The system directories (`/usr/bin`, `/bin`), which hold the linker `rustc` calls and the tools the client needs.

The developer's `~/.cargo/bin` is never on `PATH`: it holds the rustup proxies, but also anything installed with `cargo install`, including a developer's own `cargo-agents`. A pre-launch check confirms `cargo-agents` resolves nowhere on the baseline's `PATH`.

Before launch, the runner also checks that the baseline has no `~/.symposium`, no installed skills, and no Symposium entries in the client's hook or MCP configuration, and that the client's machine-wide managed-settings locations are empty. Any failed check is a setup error.

The starter has no git history, so one trial cannot read another's work through it. Codex requires a git repository unless told otherwise, so the Codex adapter passes `--skip-git-repo-check`.

The API key is passed to the client process only and excluded from every retained artifact and from grading. Each trial starts a new conversation; resume and continue options are never used. The runner refuses clients older than a pinned minimum version per client, recorded in the runner, since hook and transcript formats change between versions.

### Isolation

The fresh home and `PATH` decide what the trial is *configured* with. They do not stop a process from naming other files by absolute path, and without a boundary around the whole client, the developer's files, other trials, and the task's own held-out tests and solutions are all readable. Both validity and safety depend on that boundary, so the runner has two isolation modes and records which one each trial used.

#### Enforced: `srt`

[`srt`](https://github.com/anthropics/sandbox-runtime) is the open-source sandbox runtime that Claude Code's own sandbox is built on, packaged as a command that wraps any program and every process it starts. It uses Seatbelt on macOS and bubblewrap on Linux; its Windows support is in alpha. The runner pins an `srt` version, records it in each result, and launches each client as `srt --settings <run>/srt.json <client> ...`. `srt` itself runs outside the boundary it creates; if it needs helper files inside the boundary, they are staged into `tools/common/` like the clients.

`srt`'s read model is deny-then-allow: everything is readable unless a `denyRead` region covers it, and `allowRead` reopens paths inside a denied region. Network is denied unless allowed. The runner generates this policy per trial and retains the exact file with the trial's results:

```json
{
  "filesystem": {
    "denyRead": ["<user data roots>", "<eval-root>", "<symposium checkout>"],
    "allowRead": ["<run>", "<batch>/tools/common", "<toolchain>"],
    "allowWrite": ["<run>/project", "<run>/home", "<run>/cargo-home", "<run>/tmp", "<run>/evidence"]
  },
  "network": {
    "allowedDomains": ["<the client's model API host>"]
  }
}
```

The read policy blocks exactly these regions:

- **user data roots**: every user's home and the usual mount points (`/home`, `/root`, `/mnt`, `/media` on Linux; `/Users` and `/Volumes` on macOS), which covers the developer's home, `~/.cargo/bin`, and other checkouts;
- **the evaluation root**, so other trials and earlier batches are unreadable;
- **the Symposium checkout**, which holds `evals/`, so held-out tests, solutions, and other tasks' plugins are unreadable.

Everything else stays readable, notably the system directories (`/usr`, `/bin`, `/etc`, `/opt`) and shared temporary directories outside the evaluation root. Those hold no Symposium content or task answers, so the claims this RFD makes are limited to the regions above. A machine with user data elsewhere needs those paths added to `denyRead`, which the runner accepts as configuration. Denying `/` and re-allowing system paths is possible on Linux but not documented for macOS, so it is not the default.

A Symposium trial's policy also lists `<batch>/tools/symposium` in `allowRead`; a baseline's never does.

Writes are denied everywhere except the trial's own writable directories. The fixture crates and staged tools are read-only.

Because `srt` is the boundary, each client's own sandbox is turned off inside it, and the client runs without permission prompts: Claude Code with `--permission-mode bypassPermissions`, Codex with `--sandbox danger-full-access`. Both clients document those modes for environments that are already isolated, which an `srt` trial is. The two clients then run under the same boundary, with no nested sandboxes.

Grading an agent's submission also runs under `srt`, with its own fresh writable paths: the grading project, a build directory, a `CARGO_HOME`, and a temporary directory. The fixtures stay read-only, and there is no network and no credential in the environment.

Enforced mode is the default on macOS and Linux and requires `srt` and its dependencies (`ripgrep`, plus `bubblewrap` and `socat` on Linux). Only enforced trials appear in comparisons.

#### Exploratory: without `srt`

When `srt` is unavailable, or on Windows until its support leaves alpha, `--exploratory` runs trials with each client's own sandbox instead. Every result is labeled `"isolation": "exploratory"`, and `benchmark.json` for an exploratory batch says so at the top. Exploratory runs are for developing tasks and adapters, not for claims about Symposium.

The client sandboxes are narrower than `srt`, so exploratory mode keeps them as restrictive as each client allows:

- **Claude Code**: its sandbox covers shell commands only, and its file tools follow permission rules. The runner passes, through `--settings`, `sandbox.enabled`, `failIfUnavailable`, `allowUnsandboxedCommands: false`, `filesystem.denyRead` for the developer's home with `allowRead` for the trial and toolchain, `allowWrite` for `<run>/cargo-home`, and `permissions.blockReadsOutsideWorkingDirectories`, and runs with `--permission-mode acceptEdits`. On native Windows, Claude Code cannot sandbox shell commands at all.
- **Codex**: `--sandbox workspace-write`, with `<run>/cargo-home`, `<run>/home`, and `<run>/evidence` added as `sandbox_workspace_write.writable_roots`. Codex documents no way to restrict reads.

An escape audit then scans each transcript for paths outside the trial and for any `cargo-agents` call in a baseline trial. It parses command text, so relative paths, variables, symlinks, and scripts can evade it: an empty audit cannot certify isolation, which is why exploratory results are never compared. In enforced mode, the same audit runs as a secondary check.

### Initializing Symposium without pre-syncing

In the Symposium configuration, the runner copies the task's plugins into `<run>/registry`, writes this config, and runs `cargo agents init --add-agent <client>`:

```toml
auto-sync = true
hook-scope = "global"
auto-update = "off"

[defaults]
symposium-recommendations = false
user-plugins = false

[[registry]]
name = "eval"
path = "<run>/registry"
```

`init` registers hooks without syncing. Global scope matters: project-scoped hooks only exist after a sync, which would install skills before the behavior under test. The runner never runs `cargo agents sync`, and the prompt never mentions Symposium. Telemetry stays at its default (off).

Auto-sync failures do not fail the agent session, so a successful client exit is not evidence that sync happened. Symposium's log is retained for diagnosis.

### Same-session skill discovery

Installing a skill during `SessionStart` is not enough for the first prompt to see it. Claude Code runs skill discovery before `SessionStart` hooks finish, and rescans only when a hook returns `"reloadSkills": true` in its `SessionStart` output. Symposium's `SessionStart` handler (`handle_session_start` in `src/hook.rs`) never sets it, so today a first-session skill is installed but likely invisible until the next session.

This proposal changes Symposium: after a successful auto-sync on `SessionStart`, the Claude Code response always includes `reloadSkills: true`. Reloading unconditionally is the simplest correct change. Reloading only when skills changed would need `sync::sync` to report what it changed; it returns `Result<()>` today, and the Boolean `run_auto_sync` returns means "dependencies changed", which is not the same signal and must not be reused for it. A sync summary can replace the unconditional reload later if rescans prove costly.

For Codex, third-party sources indicate skills are scanned only at startup, and its documentation describes no reload mechanism. Step 7 determines Codex's actual behavior. If Codex cannot discover a skill installed during its own session, its `discoverable` result is a measured limitation of first-session delivery on Codex, reported as such, not hidden by pre-syncing.

### Codex Stop hook support

Codex supports `Stop` hooks, but Symposium does not register or map the event for Codex today: `register_codex_hooks` in `src/agents/mod.rs` registers four events, and `src/hook_schema/codex.rs` has no `Stop` arm. This proposal adds it, following the Claude mapping. Until it lands, the `formatting` task on Codex fails its hook check.

### Client runner

A `ClientRunner` takes a prepared trial environment, a prompt, and a deadline, and returns how the session ended. Setup and grading live outside it, so task code never branches on client-specific formats. In enforced mode, both adapters run:

```sh
srt --settings "$RUN/srt.json" claude -p "$PROMPT" --output-format stream-json --verbose \
  --model "$MODEL" --permission-mode bypassPermissions
srt --settings "$RUN/srt.json" codex exec --json --model "$MODEL" \
  --sandbox danger-full-access --skip-git-repo-check --dangerously-bypass-hook-trust "$PROMPT"
```

Codex runs a hook only after it is trusted; the trial's fresh home contains no hooks except Symposium's, so `--dangerously-bypass-hook-trust` applies to exactly those, for that one invocation.

A missing completion event or a client-reported error counts as a client error, even when the process exits 0. When a deadline expires, the runner kills the process tree, including any hooks it spawned, and keeps whatever evidence exists.

### Reuse of `symposium-testlib`

The runner reuses testlib's fixture preparation and hook trace types. It adds two explicit setup modes, baseline and Symposium-without-sync, instead of routing through `with_fixture`, whose unconditional `init` and `sync` are exactly what this evaluation must avoid. Existing simulation and SDK tests are unchanged.

Each trial's environment is set on the client's `Command`, never on the runner's own process, so the runner does not set process-wide environment variables to select a trial.

### Outcomes

Each trial reports its groups separately, so a failure points at its cause:

- **Execution**: completed, setup failure, client error, or timeout.
- **Grading**: graded, or a grading infrastructure error (submission failures are task results, not errors).
- **Isolation**: the mode (enforced or exploratory) and any audit findings.
- **Task**: each task grader, and edits made outside `editable` files.
- **Delivery and use** (Symposium configuration): installed, discoverable, and observed load, per skill.
- **Integration** (Symposium configuration): skill directory empty before launch, `SessionStart` trace, hook records.
- **Success**: by task kind, as defined above.

A missing hook or skill is a Symposium failure, not a point in the baseline's favor. A failed trial never stops the batch; every trial is kept.

### The runner in code

The runner lives in `xtask` and is synchronous: trials run one after another and only spawn subprocesses, so async would add a runtime without adding concurrency. `Err` means the runner itself broke, such as a disk that filled up. Everything that can go wrong in a trial, including a grader that hangs, is an outcome.

A trial allocates its result directory first and finalizes a result on every path, so no failure loses evidence. Each phase has its own deadline:

```rust
fn run_trial(
    task: &Task,
    client: &impl ClientRunner,
    plan: &TrialPlan,
) -> anyhow::Result<TrialResult> {
    let mut result = TrialResult::started(task, plan, ResultDir::create(plan)?);

    let run = match RunDir::prepare(task, plan, plan.setup_deadline) {
        Ok(run) => run,
        Err(err) => return result.finalize_setup_error(err),
    };
    result.execution = client.run(run.env(), task.prompt(), plan.agent_deadline);
    result.grading = grade(task, &run, plan.grading_deadline);

    // `run` is still alive here, so its evidence is copied before the directory is removed.
    result.finalize(run)
}
```

`grade` returns a value, not a `Result`. A submission that fails or hangs produces failed graders; only an infrastructure problem becomes `Grading::InfrastructureError`, and both are recorded inside the trial's result. `finalize` takes ownership of the trial directory, copies the submission and evidence into the result directory, writes `result.json`, and only then removes the trial directory.

A client adapter only builds its command line. Isolation lives in `RunEnv`, whose `command` clears the environment, sets the trial's `HOME`, `PATH`, and working directory, and wraps the program in `srt`, so adding a client means adding one small adapter:

```rust
pub trait ClientRunner {
    fn run(&self, env: &RunEnv, prompt: &str, deadline: Duration) -> Execution;
}
```

Everything except the model call is testable without a client or API key. For example, the guarantee behind "without Symposium":

```rust
#[test]
fn baseline_cannot_reach_cargo_agents() -> anyhow::Result<()> {
    let opts = RunOpts::for_test();
    let task = Task::load(&opts.evals_root, "store-durability")?;
    let run = RunDir::prepare(&task, &opts.plan(Mode::WithoutSymposium), opts.setup_deadline)?;

    let status = run
        .env()
        .command("sh")
        .args(["-c", "command -v cargo-agents || cat \"$DEVELOPER_CARGO_BIN/cargo-agents\""])
        .env("DEVELOPER_CARGO_BIN", opts.developer_cargo_bin())
        .status()?;
    assert!(!status.success());
    Ok(())
}
```

The second half of that command reads the developer's copy by absolute path, which `srt` must refuse.

### Retained results

Each trial writes to its own result directory, created before the trial starts, and each batch writes one `benchmark.json`. A trial's `result.json`:

```json
{
  "task": { "name": "store-durability", "kind": "capability", "content_hash": "<sha256 of task, fixture, and plugin dirs>" },
  "client": { "name": "claude", "version": "<version>" },
  "model": { "name": "<model>", "settings": {} },
  "mode": "with-symposium",
  "trial": 3,
  "order": 2,
  "symposium": { "version": "<cargo-agents --version>", "sha256": "<binary hash>", "revision": "<git sha or null>" },
  "os": "<os and arch>",
  "toolchain": "<rustc version>",
  "deadlines_secs": { "setup": 120, "agent": 600, "grading": 300 },
  "elapsed_secs": 94,
  "usage": { "input_tokens": 30100, "output_tokens": 4100, "cache_read_tokens": 7000, "cache_write_tokens": 0 },
  "execution": "completed",
  "isolation": { "mode": "enforced", "srt_version": "<version>", "policy": "srt.json", "audit_findings": [], "config_changes": [] },
  "task_checks": {
    "cargo_test_lib": "pass",
    "cargo_fmt_check": null,
    "held_out": { "settings_persist": { "status": "pass", "tests_run": 1, "tests_expected": 1 } },
    "edits_outside_editable": []
  },
  "delivery": { "fixture-store": { "installed": "pass", "discoverable": "pass", "observed_load": "loaded" } },
  "integration": { "skill_dir_empty_before": "pass", "session_start_trace": "pass", "hook_records": {} },
  "success": "pass"
}
```

The batch's `benchmark.json`:

```json
{
  "task": "store-durability",
  "client": "claude",
  "isolation": "enforced",
  "configurations": {
    "without-symposium": {
      "planned": 5, "setup_failures": 0, "cancelled": 0, "started": 5,
      "excluded": { "isolation": 0, "grading": 0 },
      "eligible": 5, "succeeded": 2, "failed": 3, "inconclusive": 0,
      "success_rate": 0.4, "pass_all": false,
      "cost": {
        "eligible": { "tokens": { "mean": 38900, "trials": 5 }, "agent_secs": { "mean": 81, "trials": 5 } },
        "succeeded": { "tokens": { "mean": 44100, "trials": 2 }, "agent_secs": { "mean": 92, "trials": 2 } }
      }
    },
    "with-symposium": {
      "planned": 5, "setup_failures": 0, "cancelled": 0, "started": 5,
      "excluded": { "isolation": 0, "grading": 0 },
      "eligible": 5, "succeeded": 4, "failed": 1, "inconclusive": 0,
      "success_rate": 0.8, "pass_all": false,
      "observed_load": { "fixture-store": { "loaded": 4, "attempted": 0, "not_observed": 1, "unknown": 0 } },
      "cost": {
        "eligible": { "tokens": { "mean": 35100, "trials": 5 }, "agent_secs": { "mean": 70, "trials": 5 } },
        "succeeded": { "tokens": { "mean": 36000, "trials": 4 }, "agent_secs": { "mean": 72, "trials": 4 } }
      }
    }
  },
  "delta": { "success_rate": 0.4 }
}
```

Usage the client does not report is `null`, never zero, and is left out of the means. Alongside `result.json`, each trial directory holds the submission as the agent left it, the grading project's output, the final response, the transcript, stderr, hook traces, and hook evidence. Failed trials are never retried automatically or discarded; a rerun is a new batch.

## Frequently asked questions



### What did we take from existing evaluation practice?

Published guidance on agent and skill evaluations agrees on a handful of practices, and this RFD adopts them:

- **Isolate trials.** Start each from a clean environment; [Anthropic's guide](https://anthropic.com/engineering/demystifying-evals-for-ai-agents) describes an agent that gained an edge by reading git history left by earlier trials.
- **Grade outcomes, not paths.** Check the final state, not the order of steps. [Terminal-Bench](https://arxiv.org/html/2601.11868v1) tests the final container state, whatever route the agent took.
- **Compare against a baseline, over several trials.** The [Agent Skills guide](https://agentskills.io/skill-creation/evaluating-skills) runs every test with and without the skill and reports pass rate, time, and tokens with their difference.
- **Test what the model cannot already do.** [Vercel](https://vercel.com/blog/agents-md-outperforms-skills-in-our-agent-evals) targeted APIs missing from training data. Checks that pass in both configurations measure nothing.
- **Measure whether the skill was used.** In the same evaluation, an available skill went unused in 56% of cases.
- **Include negative cases.** One-sided evaluations reward an agent that does the thing everywhere.
- **Keep graders independent and hard to game.** An agent that grades its own work can report a pass the outcome does not support, and graders that run the agent's build configuration can be changed by it.
- **Ship a reference solution.** It proves the task is solvable and the graders work; a 0% success rate usually means a broken task.

We left out LLM judges, human review, large task suites, and pass^k (the probability that all k trials succeed, which needs more trials than we start with). They matter once tasks involve judgment or the suite grows.

### Why a made-up crate?

A model may already know any real crate's conventions from training data, which makes the baseline pass and hides what the skill adds. A crate that exists only in this repository has no such leak, and its rule (writes are lost without `flush`) compiles and runs either way, so only the held-out test reveals it. It also exercises Symposium's actual feature, a skill matched to a dependency, which a plugin with `depends-on = ["*"]` never does.

The agent can still read the crate's source, and a careful one may find the rule there. That is realistic: the skill's value then shows up as lower cost rather than a higher success rate. It cannot change the crate: the copy it sees is read-only, and grading uses the original from `evals/`.

### Why grade only the editable files?

The agent's project contains everything that decides what a test means: `Cargo.toml`, build scripts, Cargo configuration, and the dependency itself. An agent could make the held-out test pass by changing any of them, for example by making `Store` flush on drop. Grading a fresh starter with only `src/` taken from the submission keeps the test's meaning fixed, and listing edits elsewhere keeps them visible.

### Why count timeouts as failures?

Excluding them rewards a configuration for failing early. If the baseline completes all five trials and succeeds in three, while Symposium times out four times and succeeds once, excluding timeouts would report 60% against 100%. Keeping them reports 60% against 20%, which matches what a user would experience. Setup failures, isolation violations, and grading infrastructure errors are different: they say nothing about the agent, so they are excluded and counted, and a batch with any of them needs inspecting. A test the agent's code makes hang is not one of these; it is a failure like any other.

### Why several trials?

Agent results vary between runs. One published measurement found ±17.6% variation on agentic tasks, larger than many differences worth detecting. Five trials per configuration is a starting point that provides limited evidence; regression and negative tasks, which should succeed every time, run once by default.

### Why `srt` and not containers?

Containers would also give an enforced boundary, but they require Docker or Podman, a base image to maintain, and Linux-only hook scripts. `srt` gives the property we need, a restriction on reads, writes, and network for a whole process tree, with no image, using the same mechanisms the clients' own sandboxes use. A container or CI virtual machine can still be added later as another way to prepare a trial, without changing the tasks, adapters, or graders.

### Why not rely on the clients' own sandboxes?

They differ by client and cover different things. Claude Code's sandbox covers shell commands but not its file tools, and does not run on native Windows. Codex restricts writes but documents no way to restrict reads. A comparison whose two clients, or two configurations, ran under different boundaries is harder to trust, and an audit of transcript text cannot close the gap. One `srt` boundary around both clients removes the difference.

### Why turn off permission checks inside `srt`?

Inside `srt`, the client's permission checks would only duplicate the boundary, and in `-p` mode every action that needs a prompt is refused, which would stop the agent from running `cargo test`. Both clients document their no-prompt modes for environments that are already isolated. Outside `srt`, in exploratory mode, the runner never uses them.

### Why is formatting a regression task and not a comparison?

Models already format Rust well, so a baseline would likely succeed every time and the comparison would show nothing. The task is still the simplest end-to-end check that a real client's `SessionStart` installs a discoverable skill and a plugin's `Stop` hook fires, which is what it is for.

### Why a Stop hook and not PostToolUse?

A Stop hook runs after the agent finishes responding. A PostToolUse hook would reformat after every edit, so the agent would always read already-formatted files and we could not tell what it wrote itself.

### Why only code graders?

Every check in the three tasks has an objective answer: a test passes, a file exists, a transcript names a skill. Code graders are fast, free, and give the same answer every time. LLM judges are needed for questions of judgment, such as whether code follows a DSL's idioms, and should be calibrated against human review when they arrive.

### Why extend the existing test harness?

It already has fixture composition, prompt execution, manual hook testing, and hook traces. The missing pieces are setup without sync, a baseline mode, native client adapters, isolation, trusted grading, and retained results. Reusing testlib avoids a second, unrelated test API.

### Why not only use simulation tests?

Simulation tests invoke hooks by hand with prepared input. They confirm Symposium handles an event correctly, not that a real client sends that event, at that time, with that shape, under its own trust rules, or that it sees the skills a hook installed. The same-session discovery gap is exactly the kind of problem simulation cannot find.

### Why not use Inspect?

[Inspect](https://inspect.aisi.org.uk/) provides scoring, logs, and sandboxing, and its [Agent Bridge](https://inspect.aisi.org.uk/agent-bridge.html) supports Claude and Codex. It is a viable alternative.

For three tasks with code graders, a Rust runner fits the existing tests and calls the clients directly, without adopting Inspect's Python task model and model proxy. If model-graded scoring and analysis become most of the work, we should revisit this before expanding the runner.

### How does this relate to the formality evaluation?

The [formality evaluation](https://github.com/rust-lang/a-mir-formality/blob/main/crates/formality-core/skills/a-mir-formality-idiomatic-judgment-fn/evals/evals.json) is a later capability task. It needs a prepared dependency, behavioral checks, and judgments about DSL idioms, which means an LLM grader. `task.toml` shares field names with its `evals.json`, so importing it is a format conversion plus that grader.

### What if the baseline also succeeds?

Record it. A capability task where both configurations always succeed has stopped measuring anything, and should be made harder or retired; the reference and naive solutions make that easy to check. The runner never requires the baseline to fail.

### What are the costs and limitations?

Real trials cost model usage and take minutes each, so a five-trial comparison on two clients is twenty sessions. Client versions come from the developer's machine, so results from different machines are comparable only when the recorded versions match. Enforced isolation needs macOS or Linux today. Observed loads show exposure to a skill, not that the agent followed it.

### What happens if we do nothing?

Comparisons stay manual, so setup and evidence collection fall on each author and results are hard to reproduce. No test covers first-session synchronization with a real client, which is the first thing every new user experiences, and which already has a known gap on Claude Code. And we have no measure of whether delivered skills are used at all.

## Implementation plan and status



### Step 1: Task format and trusted grading

Add the `evals/` layout, `task.toml` parsing, the `fixture-store` crate, and the three tasks with their starters, held-out tests, and reference and naive solutions. Add the trusted grading project, explicit held-out targets with test counts, and `--check-tasks`, and run it in CI. Test that each solution passes, that the naive solution compiles and fails exactly its `naive_fails` checks, that extra agent-written unit tests do not change a grade, and that an edit to `Cargo.toml` or the fixture crate does not either.

- [ ] Task format, trusted grading, and `--check-tasks`.



### Step 2: Isolated trial setup

Pin an `srt` version. Build the evaluation root, batch and trial directories, tool staging, the cleared environment and `PATH`, `--symposium-bin`, both setup modes with their pre-launch checks, the generated `srt` policy, and the exploratory fallback. Add result directories created before each trial and finalized on every path, and per-phase deadlines.

Add a no-model smoke test under the exact generated policy. It launches the real staged clients and `cargo-agents` with `--version`, builds and tests a project with the system linker, writes to `CARGO_HOME`, and fails to read the developer's home (including `cargo-agents` by absolute path), another trial, an earlier batch, and `evals/`. In a baseline trial, it also fails to read or run the staged binary in `tools/symposium/`. This is the first thing to build: it decides whether the runner can launch at all. Also test that a submission whose test hangs is a counted failure, that a grader that cannot launch its sandbox is an excluded infrastructure error with its evidence kept, and that a symlink under `src/` invalidates the submission.

- [ ] Trial setup, `srt` boundary, tool staging, and smoke test.



### Step 3: Same-session skill discovery for Claude Code

After a successful auto-sync on `SessionStart`, return `reloadSkills: true` in the Claude Code response. Cover it with the hook schema tests.

- [ ] `reloadSkills` support.



### Step 4: Run the regression task through Claude Code

Add `ClientRunner`, the Claude adapter, the delivery, use, and integration graders, the evidence cross-checks, and the escape audit. Test transcript parsing, client errors, timeouts, and inconclusive results against a stand-in client and recorded transcripts. Record which transcript fields, if any, list available skills. Then run `formatting` in enforced mode and confirm installation, discovery in the same session, and both hook traces. Record the minimum supported Claude Code version.

- [ ] Claude adapter and `formatting`.



### Step 5: Comparisons on Claude Code

Add `--trials`, counterbalanced ordering, success by task kind, and `benchmark.json` aggregation. Test aggregation against recorded results, including timeouts, setup failures, each kind of exclusion, inconclusive trials, and a configuration with no eligible trials, and check that the totals add up. Then run `store-durability` with five trials and `store-not-applicable` once, and record the results here.

- [ ] Trials, aggregation, and the capability and negative tasks.



### Step 6: Add Codex Stop hook support

Map and register `Stop` for Codex in `src/hook_schema/codex.rs` and `src/agents/mod.rs`, following Claude. Cover it with the existing hook schema and registration tests.

- [ ] Codex `Stop` support.



### Step 7: Run the tasks through Codex CLI

Add the Codex adapter and its skill-load detection. Reuse the tasks, graders, and isolation unchanged. Test parsing and failures without model calls, then run all three tasks. Determine whether Codex can discover a skill installed during its own `SessionStart`, and record the answer here. Record the minimum supported Codex version.

- [ ] Codex adapter.



### Step 8: Document the workflow

Finalize the usage guide with prerequisites, invocation, writing a task, and reading results. Update the running-tests and testing-guidelines chapters. Run existing tests, build all affected crates, and run `cargo xtask md-orphan-check`.

- [ ] Documentation and validation.