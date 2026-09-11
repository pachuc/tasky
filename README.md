# Tasky

A Rust task graph for agents, with a CLI for automation and a GPUI desktop viewer.
This repository is an initial scaffold, with a small working workflow and a
[detailed implementation plan](plan.md). It is not yet a production scheduler.

## What works today

- Create tasks, add/remove prerequisites, and query ready work.
- Claim a ready task for an agent, complete or fail it, and retry failed work.
- Reject cycles, missing dependencies, conflicting claims, and invalid transitions.
- Persist a versioned JSON snapshot with a local writer lock and atomic replacement.
- Use every implemented action from the CLI; emit compact JSON for agents.
- View task status, ownership, failure reasons, and prerequisite IDs in a GPUI window;
  refresh the snapshot with a button.

The viewer is a scrollable task list, not yet a node-and-edge canvas. Leases,
crashed-agent recovery, automatic refresh, task editing/deletion, cancellation,
artifacts, and event history are planned. Tasky records work; it does not execute
shell commands or launch agents.

## Quick start

Install Rust through [rustup](https://rustup.rs/). `rust-toolchain.toml` pins the
compiler and development components. Default Cargo commands build the headless
crates; GPUI is a separate workspace member so agents need no display libraries.

```sh
cargo build --locked
cargo run -p tasky-cli -- init
cargo run -p tasky-cli -- add design "Design the API"
cargo run -p tasky-cli -- add build "Implement the API"
cargo run -p tasky-cli -- depend build design
cargo run -p tasky-cli -- --json ready
cargo run -p tasky-cli -- claim design --agent agent-1
cargo run -p tasky-cli -- complete design --agent agent-1
cargo run -p tasky-cli -- --json ready
```

Or install the CLI: `cargo install --locked --path crates/tasky-cli`, then use
`tasky --help`. Data defaults to `.tasky/` under the current directory. Use
`--store /absolute/path/to/store` on both applications to share a graph.
Initialization refuses to overwrite an existing graph.

## CLI contract

Global flags: `--store PATH`, `--json`, `--help`, `--version`.

| Command | Meaning |
| --- | --- |
| `init` | Create an empty store |
| `add ID TITLE` | Add a pending task with a caller-chosen unique ID |
| `list` / `show ID` | Read all tasks / one task |
| `depend ID DEPENDENCY` | ID requires DEPENDENCY to be done |
| `undepend ID DEPENDENCY` | Remove that requirement |
| `ready` | List pending tasks with all prerequisites done |
| `claim ID --agent NAME` | Atomically move ready work to running |
| `complete ID --agent NAME` | Move owned running work to done |
| `fail ID --agent NAME --reason TEXT` | Move owned running work to failed |
| `retry ID` | Return failed work to pending |
| `snapshot` | Export the full versioned graph |
| `validate` | Check stored graph invariants |

Successful mutations return the full resulting snapshot. `list` and `ready`
return arrays, `show` returns a task, and `validate` returns `{"valid":true}`.
Output defaults to pretty JSON; `--json` selects one compact JSON value per line.
IDs sort lexicographically for deterministic output. Empty results are `[]`.
Runtime errors produce no stdout and exit 1; with `--json`, stderr contains
`{"error":{"message":"..."}}`. CLI syntax errors use Clap's text diagnostics and
exit 2; help/version exit 0. Error messages are not stable machine error codes yet.

`ready` is advisory: another agent may claim a task before you do. Always check
the result of `claim`. Agent names are ownership labels, not authentication.
Dependencies can change only while a task is pending. Done tasks are immutable;
failed tasks need an explicit retry. There are no claim timeouts in this scaffold.

## Desktop viewer

The UI uses the published [GPUI 0.2.2 crate](https://docs.rs/gpui/0.2.2/gpui/).
Start with an initialized store, then:

```sh
cargo run --locked -p tasky-ui -- --store .tasky
```

GPUI requires native desktop build dependencies and a working GPU/display session.
On Linux, install the development packages for X11/XCB, Wayland, xkbcommon,
fontconfig, OpenSSL, and Vulkan, plus a C/C++ compiler, CMake and pkg-config.
Consult GPUI's [Linux platform documentation](https://github.com/zed-industries/zed/tree/main/docs/src/development)
for platform setup; upstream requirements may evolve. macOS requires Xcode command
line tools. CI checks the UI on macOS; Windows support is not validated.
The viewer loads at startup and on **Refresh snapshot**. A refresh error retains
and labels the last successful snapshot. Close the last window to exit.

## Development

```sh
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
# Requires desktop development dependencies:
cargo clippy --locked --workspace --all-targets -- -D warnings
```

Every crate inherits the workspace lint policy from `Cargo.toml`: Rust forbids
unsafe code in our crates, and Clippy denies its default and full pedantic groups
plus `redundant_clone`, `needless_collect`, and `large_stack_frames`. These Clippy
rules fail even without `-D warnings`; that flag also rejects other compiler
warnings. New workspace crates must include `[lints]` with `workspace = true`.
The unsafe-code restriction does not apply to third-party dependencies.

| Crate | Responsibility |
| --- | --- |
| `tasky-core` | Task model, DAG validation, readiness and transitions |
| `tasky-store` | Snapshot persistence and serialized local mutations |
| `tasky-cli` | CLI arguments and JSON presentation (`tasky` binary) |
| `tasky-ui` | Read-only GPUI presentation (`tasky-ui` binary) |

[GitHub Actions CI](.github/workflows/ci.yml) runs headless tests and checks on Linux
and Clippy for the entire workspace, including the UI, on macOS. The same workflow
is provided at [`ci/github-actions.yml.example`](ci/github-actions.yml.example).

The committed lockfile covers the whole workspace. Storage currently targets
small graphs on a local filesystem; all writers must use the store API. Network
filesystems, direct concurrent JSON edits, and multi-host coordination are not
supported. See [plan.md](plan.md) for architecture, milestones, and acceptance tests.
