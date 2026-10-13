# Proposed guide: evaluating agents with and without Symposium

This page describes the workflow proposed by [the skill evaluation RFD](./README.md). The runner is not implemented yet.

## Prerequisites

- macOS or Linux.
- The client under test (`claude` or `codex`), at or above the runner's minimum supported version, and a Rust toolchain with `rustfmt`.
- [`srt`](https://github.com/anthropics/sandbox-runtime), at the version the runner pins, and its dependencies: `ripgrep`, plus `bubblewrap` and `socat` on Linux.
- An API key for the client under test, exported as that client's usual environment variable.

Checking tasks needs only the Rust toolchain, since it grades this repository's own reference solutions rather than agent output. Real evaluations call a model and are never part of `cargo test`.

## Check the tasks

```sh
cargo xtask agent-eval --check-tasks
```

This grades every task's reference solution and confirms it passes, and grades each capability task's naive solution and confirms it compiles and fails exactly the checks its `naive_fails` lists. It also records how many tests each held-out target runs, which later trials must match. It calls no model and runs in CI. Run it after changing any task.

## Run a task

```sh
cargo xtask agent-eval store-durability --client claude --model <model> --trials 5
cargo xtask agent-eval formatting --client codex --model <model>
```

`--client` is `claude` or `codex`. `--trials` defaults to 1. `--timeout` sets the agent's deadline in seconds (default 600); setup and grading have their own. The runner builds `cargo-agents` from the current checkout first; `--symposium-bin <path>` evaluates another binary instead, such as an installed release.

A capability task runs both configurations, without and with Symposium, alternating which goes first. Regression and negative tasks run only with Symposium; pass `--baseline` to run the baseline too.

Your installed `cargo-agents` can stay installed. Each trial runs inside `srt`, which makes every user's home and the usual mount points, all evaluation batches other than its own trial, and the Symposium checkout (including the held-out tests) unreadable. It can write only its own directory and reach only the model API. System directories stay readable. If you keep data outside those regions, add it to the runner's deny list. The runner copies the client and `cargo-agents` into the batch once, since a sandboxed trial cannot launch them from your home. Only Symposium trials are given the binary under evaluation, and Symposium is initialized but never synced: skills are installed by the agent's own `SessionStart` hook, which is part of what the trial checks.

Without `srt`, `--exploratory` runs trials with each client's own sandbox instead. Exploratory results are labeled as such and are for developing tasks and adapters, not for comparisons.

## Write a task

Pick the kind first:

- **capability**: does Symposium improve the outcome? The task must need knowledge the model cannot already have, or both configurations will succeed and the comparison shows nothing. A fixture crate in this repository is the usual way.
- **regression**: does Symposium still deliver what it should? Expected to succeed every time.
- **negative**: does Symposium stay out where it does not apply?

A task is a directory under `evals/tasks/`:

```text
evals/tasks/store-durability/
  task.toml      # kind, plugins, fixtures, editable files, prompt, expectations
  starter/       # project copied into each trial
  checks/        # held-out tests, one file each; the agent never sees them
  solution/      # files copied over the starter; must pass every check
  naive/         # capability tasks only; must fail exactly the naive_fails checks
```

Plugins live under `evals/plugins/` and fixture crates under `evals/fixtures/`, named in `task.toml`, so several tasks can share them. Fixture crates are read-only in a trial.

Only the files `editable` names (default `src/**`) are graded, in a fresh copy of the starter. Keep everything a test's meaning depends on, such as dependencies and build configuration, outside `editable`.

Grade the outcome: a held-out test that exercises the behavior is better than a check of how the code looks. Write the naive solution the way an agent without the skill plausibly would; it should fail the check for the mistake the skill prevents and nothing else.

## Read the results

```text
results/store-durability/claude/<batch>/
  benchmark.json
  trial-1-without-symposium/
    result.json
    submission/          # the project as the agent left it
    grading/             # the trusted grading project's output
    transcript.jsonl
    response.txt
    stderr.log
  trial-1-with-symposium/
    ...                  # as above, plus:
    hook-trace.jsonl
    evidence/
    symposium.log
  ...
```

Read a batch in this order:

1. **Isolation.** If `benchmark.json` says `exploratory`, the results are not a comparison. In an enforced batch, any `excluded.isolation` count or audit finding is a reason to inspect the batch before anything else.
2. **Execution.** Setup failures and grading infrastructure errors are excluded and counted; inspect them first, since they usually mean a broken environment. An excluded grading error keeps the submission, so it can be graded again. Timeouts, client errors, and code that fails, crashes, or hangs under grading count as failures.
3. **Delivery.** In the Symposium configuration, each expected skill should be *installed* and *discoverable* in the same session, with a `SessionStart` trace and the expected hook records. Each result is `pass`, `fail`, or `inconclusive`; inconclusive means the transcript could not decide, usually because the client does not list its available skills and the agent did not load the skill. Installed but not discoverable means the client did not see a skill installed during its own session. Hook records are written inside the trial, so treat them as supporting evidence; `config_changes` and the transcript show whether anything altered them. `symposium.log` shows why a sync failed.
4. **Use.** The `observed_load` counts show how often the transcript showed the agent loading a skill: `loaded`, `attempted`, `not_observed`, or `unknown`. A load shows the agent saw the skill, not that it followed it. A skill that is delivered but rarely loaded points at its name or description, not its content.
5. **Outcome and cost.** For a capability task, compare `success_rate` between configurations; timeouts and client errors count as failures. Compare cost over succeeded trials as well as over all eligible ones: a configuration that is cheaper only because it fails quickly has not saved anything. Each cost figure says how many trials it comes from. `edits_outside_editable` in a trial's result shows changes the grade ignored.

Then read some transcripts. Numbers alone do not show whether a failure was the agent's mistake or a grader rejecting a valid answer.

Five trials per configuration provide limited evidence. Treat results as something to look into, not as a measurement. A capability task where both configurations always succeed has stopped measuring anything; make it harder or retire it.

A rerun is a new batch, so earlier evidence is kept. Credentials never appear in the results.
