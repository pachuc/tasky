-- IDs are ULIDs as 26-character text. Timestamps are RFC 3339 in UTC.
-- One database holds any number of projects. A project is anything work is organized
-- under; the repo columns are set only when it is tied to a repository, on disk or remote.
CREATE TABLE projects (
    id           TEXT PRIMARY KEY,
    slug         TEXT NOT NULL UNIQUE,
    name         TEXT NOT NULL,
    repo_path    TEXT,
    repo_url     TEXT,
    created_at   TEXT NOT NULL
);

-- A goal belongs to one project; slugs are unique within a project.
-- The spec is optional free text describing the goal in detail.
CREATE TABLE goals (
    id           TEXT PRIMARY KEY,
    project_id   TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    slug         TEXT NOT NULL,
    title        TEXT NOT NULL,
    description  TEXT NOT NULL DEFAULT '',
    spec         TEXT,
    status       TEXT NOT NULL DEFAULT 'draft'
                 CHECK (status IN ('draft', 'active', 'complete', 'cancelled')),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    completed_at TEXT,
    UNIQUE (project_id, slug)
);

-- Lifecycle: todo -> in_progress <-> testing -> ready_for_merge -> done, or cancelled.
-- The test plan lists the validation steps; pr links the pull request that delivers the task.
CREATE TABLE tasks (
    id           TEXT PRIMARY KEY,
    goal_id   TEXT NOT NULL REFERENCES goals(id) ON DELETE CASCADE,
    title        TEXT NOT NULL,
    body         TEXT NOT NULL DEFAULT '',
    test_plan    TEXT NOT NULL DEFAULT '',
    pr           TEXT,
    status       TEXT NOT NULL DEFAULT 'todo'
                 CHECK (status IN ('todo', 'in_progress', 'testing', 'ready_for_merge',
                                   'done', 'cancelled')),
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    completed_at TEXT
);
CREATE INDEX tasks_goal_id_idx ON tasks (goal_id);
CREATE INDEX tasks_status_idx     ON tasks (status);

-- task_id requires depends_on_id to be done first. Acyclicity and the rule that both tasks
-- belong to the same project are enforced in Rust.
CREATE TABLE task_dependencies (
    task_id       TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    depends_on_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    PRIMARY KEY (task_id, depends_on_id),
    CHECK (task_id <> depends_on_id)
);
CREATE INDEX task_dependencies_depends_on_idx ON task_dependencies (depends_on_id);

-- Pinned commits and arbitrary URLs attached to a task.
CREATE TABLE task_links (
    id           TEXT PRIMARY KEY,
    task_id      TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    kind         TEXT NOT NULL CHECK (kind IN ('commit', 'url')),
    reference    TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    UNIQUE (task_id, kind, reference)
);
