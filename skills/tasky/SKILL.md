---
name: tasky
description: Plan and track engineering work with the tasky CLI - use when decomposing a plan into projects, goals, and tasks; recording a plan that has been agreed with the user; picking up, progressing, or finishing tasks (start, test, pass, pr, done); checking what is ready or blocked; or whenever tasky, goals, task plans, or a task graph are mentioned.
---

# Tasky for agents

Tasky is a local task graph. One SQLite database, outside every repository, holds any
number of **projects**; a project holds **goals**; a goal holds **tasks**. Projects and goals
nest without limit, tasks do not. Tasks depend on other tasks to form a DAG. You drive all
of it through the `tasky` CLI. There is also `tasky ui`, a graph viewer for humans; you never
need it.

The full command surface is in [reference.md](reference.md), generated from the binary.
Read it for exact flags. This file is about using Tasky well.

## The one rule about when to write

**Only put a plan into Tasky once it is aligned.** If you are planning with the user in the
loop, keep the plan in the conversation while it is being shaped. Do not create projects,
goals, or tasks for a partial or draft plan. When the user confirms the whole plan, then
break it down and compose it into Tasky in one pass. A half-written plan in Tasky is worse
than none: other agents will start executing it.

Corollaries:

- Never create tasks speculatively "to see how it looks".
- If the plan changes materially after it was recorded, say so, agree the change, then edit
  Tasky to match. Cancel what no longer applies rather than leaving it to rot.
- Recording status on tasks you are executing is not planning; do that immediately.

## Model in brief

| Thing | What it is | Status |
| --- | --- | --- |
| Project | Anything work is organized under. May have a parent project. May record a repo path and URL. | none |
| Goal | A high-level outcome inside a project, with an optional spec. May have a parent goal in the same project. | `draft → active → complete`, or `cancelled` |
| Task | The unit of work: title, body, test plan, pull request, dependencies, links. | `todo → in_progress ⇄ testing → ready_for_merge → done`, or `cancelled` |

Rules the CLI enforces, so plan around them:

- A task can `start` only when every dependency is `done`. Cancelled dependencies keep
  dependents blocked until the edge is removed.
- No task state can be skipped. `done` means merged and is reachable only from
  `ready_for_merge`. `done` and `cancelled` are terminal.
- Dependencies can be edited only while a task is `todo`. They may join any two tasks under
  the same root project, sub-projects included, never across root projects. Cycles are
  rejected.
- A goal completes only when its own tasks are all `done` or `cancelled` and every sub-goal
  is `complete` or `cancelled`. Cancelling a goal also waits for its sub-goals to close.
- Closed goals accept no new tasks or sub-goals. Titles, bodies, test plans, and pull
  requests can change until a task is `done` or `cancelled`.
- "Ready" and "blocked" are derived, never stored.

## Addressing things

- Projects: by path from the root, `app` or `app/mobile/ios`. Slugs are lowercase letters,
  digits, and inner hyphens, unique among siblings.
- Goals: `PROJECT/slug` using that project path, for example `app/mobile/shell`. A bare slug
  works only while no other project uses it. Goal slugs are unique within a project at any
  depth, so the path never includes the parent goal.
- Tasks: by any unique prefix or suffix of the ULID. The last six characters are the
  random part, so use those.
- Always pass `--json`. Every mutation prints the affected record; capture the `id` from it
  instead of re-listing. Errors go to stderr as `{"error":{"message":"..."}}` with exit 1;
  a syntax error exits 2. Runtime errors print nothing on stdout.

## Composing an aligned plan

Do this once, after alignment, top down:

1. **Project.** Reuse an existing one if it exists (`tasky --json project list`). Create
   sub-projects only for genuinely separate deliverables that share a root, such as
   platforms or services. Record the repo with `--repo-path` and `--repo-url` for code.
2. **Goals.** One goal per outcome the user would recognize, typically one per feature or
   milestone. Put the agreed design in the spec: `--spec-file` for anything longer than a
   sentence. Use sub-goals when an outcome has independently completable parts; do not use
   them as folders. Activate a goal when work on it starts, not when it is created.
3. **Tasks.** One task per unit that a single agent can take from start to a mergeable
   change. Give every task a body saying what to build and a test plan saying how it will
   be validated; a coding task without a test plan is not finished planning. Titles are
   imperative and specific: "Add parent_id to goals", not "Goals work".
4. **Dependencies.** Add an edge only where order genuinely matters. The graph is what
   lets several agents work in parallel; over-connecting it serializes everyone. Prefer a
   few short chains over one long one.
5. **Check it.** `tasky --json task order --project P` should read as a sensible sequence,
   and `tasky --json task ready --project P` should show the tasks that can start right now.

Example, after alignment:

```sh
tasky --json project add app "App" --repo-path . --repo-url https://github.com/org/app
tasky --json goal add app auth "Authentication" --spec-file /tmp/auth-spec.md
tasky --json goal activate app/auth
A=$(tasky --json task add app/auth "Design the session model" \
      --body "Decide token format and expiry." --test-plan "Design doc reviewed and linked." \
      | jq -r .id)
B=$(tasky --json task add app/auth "Implement login endpoint" \
      --body "POST /login per the spec." --test-plan "cargo test passes; manual login works." \
      | jq -r .id)
tasky --json task depend "$B" "$A"
```

## Executing tasks

The loop for an agent picking up work:

1. `tasky --json task ready --project P` lists todo tasks whose dependencies are done. It
   is advisory: another agent may take one first, and `start` will then fail. Handle that
   by picking the next one.
2. `tasky --json task start ID` claims it. Read `tasky --json task show ID` for the body,
   test plan, and neighbours before working.
3. Do the work. Record the pull request as soon as it exists:
   `tasky --json task pr ID https://...`. Attach commits or other references with
   `task link ID --commit SHA` or `--url URL`.
4. `tasky --json task test ID` when the change is ready to be validated against the test
   plan. If validation fails, `task fail ID` returns it to in progress; fix and `test`
   again. When it passes, `task pass ID`.
5. `tasky --json task done ID` only once the change is actually merged.
6. When every task under a goal is closed, `tasky --json goal complete P/slug`. Complete
   sub-goals before their parents.

If a task turns out to be wrong or unnecessary, `task cancel ID` and tell the user; do not
silently mark it done. Remember cancelled prerequisites still block dependents, so
`task undepend` the edge if the dependent should proceed.

## Reading state

- `tasky --json project show P` and `goal show P/slug` give task totals over the whole
  subtree, useful for progress reports.
- `tasky --json task list --project P --status in_progress` shows what is being worked.
- `--project` and `--goal` filters include everything nested beneath.
- `tasky --json task show ID` lists `depends_on`, `blocked_by`, and `dependents` by id.

## Things not to do

- Do not record a plan that is still being discussed.
- Do not invent statuses or skip transitions; the CLI will refuse, and retrying with a
  different verb to "make it go through" corrupts the record.
- Do not mark `done` before merge, and do not mark `pass` without running the test plan.
- Do not create a goal per task or a task per file. Goals are outcomes; tasks are mergeable
  units.
- Do not delete or reset the database to fix a mistake; cancel and re-create instead so the
  history stays honest.
