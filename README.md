# Tasky

Track any number of projects, each made of goals with a DAG of tasks, in one SQLite
database kept outside your repositories. A project can be anything: a codebase, a research
effort, a launch. A CLI drives the workflow and emits JSON for
automation; a GPUI desktop viewer shows the graph. This is an early scaffold with a working
workflow.

## Model

- A **project** is anything work is organized under, with a slug humans type, such as `app`.
  Projects nest without limit: a sub-project is a full project whose parent is another
  project, addressed by path such as `app/mobile/ios`. Slugs are unique among siblings. A
  coding project may record the repository it is tied to as a local path, a remote URL, or
  both; nothing requires either. One database holds any number of projects.
- A **goal** is a high-level goal within a project, with a slug that is unique inside that
  project at any depth. Goals nest without limit too: a sub-goal is a full goal whose parent
  is another goal of the same project. A goal may carry an optional **spec**: free text
  describing it in detail.
- A **task** is the unit of work. Every task belongs to a goal. It carries a title, a body,
  a **test plan** listing the validation steps that prove it complete, and the link to the
  **pull request** that delivers it once one exists. Tasks depend on other tasks anywhere in
  the same root project, sub-projects included, forming a DAG that is kept acyclic on every
  write. Dependencies never cross from one root project to another.
- A **link** attaches a commit SHA or URL to a task.

Goals go `draft → active → complete` or `cancelled`, and a complete goal can be reopened to
active when more work turns up. A goal completes only when every task
directly under it is done or cancelled and every sub-goal is complete or cancelled, so a
parent can never close ahead of its children. Cancelling likewise waits for sub-goals to
close. A spec can be written, replaced, or cleared while its goal is draft or active.

Tasks follow one path:

```
todo → in_progress ⇄ testing → ready_for_merge → done
```

`start` needs every dependency done. `test` hands the work over to validation against the
test plan; `fail` sends it back to in progress and `pass` marks it ready for merge. `done`
records the merge and is allowed only from `ready_for_merge`. `cancel` works from any state
that is not done or cancelled. No state can be skipped, and done and cancelled are terminal.
"Blocked" and "ready" are derived, never stored. Dependencies can change only while a task is
`todo`; the title, body, test plan, and pull request can change until the task is done or cancelled.

IDs are ULIDs. Refer to a task by any unique prefix or suffix of its ID; the tail is the
random part, so the last few characters are the easiest to type. Refer to a project by slug.
Refer to a project by its path from the root, such as `app` or `app/mobile`. Refer to a
goal as `PROJECT/slug` with that same project path, by its slug alone when no other project
uses it, or by an ID fragment. `--project` and `--goal` filters include everything nested
beneath the project or goal named.

## Quick start

Install Rust through [rustup](https://rustup.rs/). `rust-toolchain.toml` pins the
compiler and development components. SQLite is compiled in. The viewer is built into the
`tasky` binary, so building needs GPUI's native development packages; see
[Desktop viewer](#desktop-viewer).

```sh
cargo build --locked
alias tasky="$PWD/target/debug/tasky"
tasky init
tasky project add app "My App" --repo-url https://github.com/you/app   # repo flags are optional
tasky goal add app auth "Authentication" --spec-file docs/auth.md   # spec is optional
tasky goal activate app/auth
tasky --json task add app/auth "Design the API"      # note the returned id
tasky --json task add app/auth "Implement the API" --test-plan "cargo test passes"
tasky task depend <implement-id> <design-id>
tasky task ready --project app
tasky task start <design-id>
tasky task test <design-id>
tasky task pass <design-id>
tasky task done <design-id>
tasky task start <implement-id>
tasky task pr <implement-id> https://github.com/you/app/pull/42
tasky task test <implement-id>
tasky task fail <implement-id>            # validation found a problem
tasky task test <implement-id>
tasky task pass <implement-id>
tasky task done <implement-id>            # merged
tasky goal complete app/auth
tasky ui                                  # open the graph viewer
```

Or install the CLI: `cargo install --locked --path crates/tasky-cli`.

## Where the database lives

The database is not part of any repository. By default it is `tasky/tasky.db` under
`$XDG_DATA_HOME`, which falls back to `~/.local/share`, so every project you track shares
`~/.local/share/tasky/tasky.db`. Override the location with `--db PATH` on either application
or the `TASKY_DB` environment variable. `init` creates the file and its parent directories and
refuses to overwrite an existing database.

## CLI contract

Global flags: `--db PATH` (or `TASKY_DB`), `--json`, `--help`, `--version`.

| Command | Meaning |
| --- | --- |
| `init` | Create an empty database and its schema |
| `migrate` | Apply pending schema migrations to an existing database |
| `ui` | Open the graph viewer on the database; returns when the window closes |
| `project add SLUG [NAME] [--parent PROJECT] [--repo-path PATH] [--repo-url URL]` | Create a project, optionally inside another; the name defaults to the slug |
| `project list` / `project show PROJECT` | List every project / show one with its path, sub-project and goal counts, and task totals over its subtree |
| `project repo PROJECT [--path PATH \| --clear-path] [--url URL \| --clear-url]` | Change the repository path and/or URL; unmentioned fields keep their value |
| `goal add PROJECT SLUG TITLE [--parent GOAL] [--description TEXT] [--spec TEXT \| --spec-file PATH]` | Create a draft goal, optionally inside another goal of the project and optionally with a spec |
| `goal list [--project P]` | List goals, optionally within one project and its sub-projects |
| `goal show GOAL` | Show a goal with its project path, sub-goal count, spec, and task totals over its subtree |
| `goal spec GOAL [--text TEXT \| --file PATH \| --clear]` | Replace the spec (stdin when no flag is given) or remove it |
| `goal activate GOAL` | Draft → active |
| `goal complete GOAL` | Active with all tasks finished → complete |
| `goal reopen GOAL` | Complete → active, so more tasks can be added; the parent goal must be open |
| `goal cancel GOAL` | Draft or active → cancelled |
| `task add GOAL TITLE [--body TEXT \| --body-file PATH] [--test-plan TEXT \| --test-plan-file PATH]` | Add a todo task to a goal |
| `task title TASK TITLE` | Replace the title |
| `task body TASK [--text TEXT \| --file PATH]` | Replace the body (stdin when no flag is given) |
| `task test-plan TASK [--text TEXT \| --file PATH]` | Replace the validation steps (stdin when no flag is given) |
| `task pr TASK URL` / `task pr TASK --clear` | Record or remove the pull request that delivers the task |
| `task list [--project P] [--goal F] [--status S]` | List tasks in ID order |
| `task show TASK` | Task with project, goal, readiness, dependencies, blockers, dependents, links |
| `task ready [--project P] [--goal F]` | Todo tasks whose dependencies are all done |
| `task order [--project P] [--goal F]` | Every task in dependency order, prerequisites first |
| `task depend TASK DEPENDS_ON` | TASK requires DEPENDS_ON to be done; rejects cycles and cross-project edges |
| `task undepend TASK DEPENDS_ON` | Remove that requirement |
| `task start TASK` | Todo with all dependencies done → in progress |
| `task test TASK` | In progress → testing |
| `task fail TASK` | Testing → in progress |
| `task pass TASK` | Testing → ready for merge |
| `task done TASK` | Ready for merge → done |
| `task cancel TASK` | Any open state → cancelled |
| `task link TASK --commit SHA \| --url URL` | Attach an external reference |

Mutations return the affected record. Lists return arrays. Output defaults to pretty JSON;
`--json` selects one compact JSON value per line. Runtime errors produce no stdout and exit
1; with `--json`, stderr contains `{"error":{"message":"..."}}`. CLI syntax errors use Clap's
text diagnostics and exit 2. Error messages are not stable machine error codes yet.

`task ready` is advisory: another process may start a task first. Every mutation runs in one
SQLite immediate transaction, so two processes cannot both start the same task.

## Desktop viewer

The viewer is part of the `tasky` binary and uses the published
[GPUI 0.2.2 crate](https://docs.rs/gpui/0.2.2/gpui/). Start with an initialized database,
then:

```sh
tasky ui                                  # honours --db and TASKY_DB like every command
```

GPUI requires native desktop build dependencies and a working GPU/display session.
On Linux, install the development packages for X11/XCB, Wayland, xkbcommon,
fontconfig, OpenSSL, and Vulkan, plus a C/C++ compiler, CMake and pkg-config.
Consult GPUI's [Linux platform documentation](https://github.com/zed-industries/zed/tree/main/docs/src/development)
for platform setup; upstream requirements may evolve. macOS requires Xcode command
line tools. CI checks the UI on macOS; Windows support is not validated.
The viewer draws every project, goal, and task as one monochrome graph of circles: projects
in the first column, their goals in the second, and tasks in the columns after that by
dependency rank, so every edge flows left to right. Thin edges are hierarchy; thicker arrowed
edges are dependencies. Everything known about a node is written inside its circle and
revealed by zoom: zoomed out you see only the shape of the graph, zoomed in you see titles,
then state and ids, then bodies, test plans, and pull requests as space allows. Drag to pan
and scroll to zoom. Nothing else is on screen; restart the viewer to pick up changes made
with the CLI.

## Development

```sh
cargo fmt --all -- --check
cargo test --locked --workspace
cargo clippy --locked --workspace --all-targets -- -D warnings
```

All of these compile GPUI, so they need the desktop development packages listed above
even on a machine without a display. Dependencies are optimized in dev builds, so the
viewer is smooth from a plain `cargo run`.

Every crate inherits the workspace lint policy from `Cargo.toml`: Rust forbids
unsafe code in our crates, and Clippy denies its default and full pedantic groups
plus `redundant_clone`, `needless_collect`, and `large_stack_frames`. These Clippy
rules fail even without `-D warnings`; that flag also rejects other compiler
warnings. New workspace crates must include `[lints]` with `workspace = true`.
The unsafe-code restriction does not apply to third-party dependencies, including
the bundled SQLite C library.

| Crate | Responsibility |
| --- | --- |
| `tasky-core` | Domain types, lifecycles, DAG rules; no I/O or clock |
| `tasky-store` | Diesel/SQLite persistence, embedded migrations, transactional operations |
| `tasky-cli` | The `tasky` binary: arguments, JSON output, and the `ui` subcommand |
| `tasky-ui` | Read-only GPUI viewer as a library, launched by `tasky ui` |

## Agent skill

`skills/tasky/` is a Claude Code skill that teaches agents the model, the addressing rules,
and how to plan and execute work with the CLI, including the rule that a plan is recorded
only once it is aligned with the user. Install it for every session with a symlink:

```sh
ln -sfn "$PWD/skills/tasky" ~/.claude/skills/tasky
```

`skills/tasky/reference.md` is generated from the binary by `scripts/update-skill-reference.sh`
and checked in CI, so it never drifts from the commands. Rerun the script after changing the
CLI.

Schema changes are Diesel migrations under `crates/tasky-store/migrations`, embedded in the
binary. Only `tasky init` and `tasky migrate` change a database's schema; every other
command, and the viewer, refuse to open a database with pending migrations and name the
fix. `crates/tasky-store/src/schema.rs` is maintained by hand to match; the `diesel` CLI
is not required.

[GitHub Actions CI](.github/workflows/ci.yml) runs tests and checks on Linux, installing
GPUI's build packages first, and Clippy for the whole workspace on macOS. The same workflow
is provided at [`ci/github-actions.yml.example`](ci/github-actions.yml.example).
