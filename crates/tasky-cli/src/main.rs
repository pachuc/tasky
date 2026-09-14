use anyhow::{Context, Result};
use clap::{Args, CommandFactory, Parser, Subcommand};
use serde_json::{Value, json};
use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use tasky_core::{LinkKind, TaskStatus};
use tasky_store::{Change, Store, TaskFilter, TaskScope, default_path};

#[derive(Parser)]
#[command(
    name = "tasky",
    version,
    about = "Track projects of any kind, their goals, and a DAG of tasks",
    after_help = "Projects are referenced by path, such as app or app/mobile, or by a unique \
                  prefix or suffix of their ID. Goals are referenced as PROJECT/SLUG, as a \
                  slug that is unique across projects, or by an ID fragment. Tasks are \
                  referenced by ID fragment."
)]
struct Cli {
    /// Path to the SQLite database
    #[arg(long, global = true, env = "TASKY_DB", default_value_os_t = default_path())]
    db: PathBuf,
    /// Emit compact machine-readable JSON (errors go to stderr as JSON too)
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create an empty database; refuses to overwrite one
    Init,
    /// Apply pending schema migrations to an existing database
    Migrate,
    /// Open the graph viewer on this database; returns when the window closes
    Ui,
    /// Print the help of every command as Markdown; feeds the agent skill's reference
    #[command(hide = true)]
    Reference,
    /// Anything work is organized under; optionally tied to a repository
    #[command(subcommand)]
    Project(ProjectCommand),
    /// High-level goals within a project, each with an optional spec
    #[command(subcommand)]
    Goal(GoalCommand),
    /// Units of work that form a dependency graph
    #[command(subcommand)]
    Task(TaskCommand),
}

#[derive(Subcommand)]
enum ProjectCommand {
    /// Create a project, optionally inside another project
    Add {
        /// Lowercase letters, digits, and inner hyphens; unique among siblings
        slug: String,
        /// Human-readable name; defaults to the slug
        name: Option<String>,
        /// Parent project path, such as app or app/mobile
        #[arg(long)]
        parent: Option<String>,
        /// Local checkout of the repository, for coding projects
        #[arg(long)]
        repo_path: Option<String>,
        /// Remote URL of the repository, for coding projects
        #[arg(long)]
        repo_url: Option<String>,
    },
    /// List all projects
    List,
    /// Change the repository path and/or URL a project is tied to
    Repo {
        project: String,
        #[command(flatten)]
        change: RepoChange,
    },
    /// Show a project with its goal count and task totals
    Show { project: String },
}

#[derive(Subcommand)]
enum GoalCommand {
    /// Create a draft goal in a project, optionally inside another goal of that project
    Add {
        project: String,
        /// Lowercase letters, digits, and inner hyphens; unique within the project
        slug: String,
        title: String,
        /// Parent goal, such as app/auth
        #[arg(long)]
        parent: Option<String>,
        #[arg(long, default_value = "")]
        description: String,
        #[command(flatten)]
        spec: SpecSource,
    },
    /// List goals, optionally within one project
    List {
        #[arg(long)]
        project: Option<String>,
    },
    /// Show a goal with its spec and task counts
    Show { goal: String },
    /// Replace the spec from --text, --file, or stdin, or clear it with --clear
    Spec {
        goal: String,
        #[command(flatten)]
        source: TextSource,
        /// Remove the spec
        #[arg(long, conflicts_with_all = ["text", "file"])]
        clear: bool,
    },
    /// Move a draft goal into active work
    Activate { goal: String },
    /// Complete an active goal whose tasks are all finished
    Complete { goal: String },
    /// Cancel a draft or active goal
    Cancel { goal: String },
}

/// At least one repository field to set or clear; unmentioned fields keep their value.
#[derive(Args)]
#[group(required = true, multiple = true)]
struct RepoChange {
    /// Local checkout of the repository
    #[arg(long, conflicts_with = "clear_path")]
    path: Option<String>,
    /// Remote URL of the repository
    #[arg(long, conflicts_with = "clear_url")]
    url: Option<String>,
    /// Remove the local path
    #[arg(long)]
    clear_path: bool,
    /// Remove the remote URL
    #[arg(long)]
    clear_url: bool,
}

fn change(value: Option<String>, clear: bool) -> Change {
    match value {
        Some(value) => Change::Set(value),
        None if clear => Change::Clear,
        None => Change::Keep,
    }
}

impl RepoChange {
    fn into_parts(self) -> (Change, Change) {
        (
            change(self.path, self.clear_path),
            change(self.url, self.clear_url),
        )
    }
}

/// Optional spec given at creation time.
#[derive(Args)]
#[group(multiple = false)]
struct SpecSource {
    /// Spec text given inline
    #[arg(long)]
    spec: Option<String>,
    /// Read the spec from a file
    #[arg(long)]
    spec_file: Option<PathBuf>,
}

#[derive(Args)]
#[group(multiple = false)]
struct TextSource {
    /// Text given inline
    #[arg(long)]
    text: Option<String>,
    /// Read the text from a file
    #[arg(long)]
    file: Option<PathBuf>,
}

#[derive(Subcommand)]
enum TaskCommand {
    /// Add a todo task to a goal
    Add {
        goal: String,
        title: String,
        #[command(flatten)]
        body: BodySource,
        #[command(flatten)]
        test_plan: TestPlanSource,
    },
    /// Replace the title
    Title { task: String, title: String },
    /// Replace the body from --text, --file, or stdin
    Body {
        task: String,
        #[command(flatten)]
        source: TextSource,
    },
    /// Replace the validation steps from --text, --file, or stdin
    TestPlan {
        task: String,
        #[command(flatten)]
        source: TextSource,
    },
    /// Record the pull request that delivers the task, or remove it with --clear
    Pr {
        task: String,
        /// Pull request URL or reference
        #[arg(required_unless_present = "clear")]
        pr: Option<String>,
        #[arg(long, conflicts_with = "pr")]
        clear: bool,
    },
    /// List tasks, optionally filtered
    List {
        #[command(flatten)]
        scope: Scope,
        #[arg(long)]
        status: Option<TaskStatus>,
    },
    /// Show a task with readiness, dependencies, dependents, and links
    Show { task: String },
    /// Todo tasks whose dependencies are all done
    Ready {
        #[command(flatten)]
        scope: Scope,
    },
    /// All tasks in dependency order, prerequisites first
    Order {
        #[command(flatten)]
        scope: Scope,
    },
    /// Require `DEPENDS_ON` to be done before `TASK` can start
    Depend { task: String, depends_on: String },
    /// Remove that requirement
    Undepend { task: String, depends_on: String },
    /// Move a ready task to in progress
    Start { task: String },
    /// Hand an in-progress task over to validation against its test plan
    Test { task: String },
    /// Send a testing task back to in progress
    Fail { task: String },
    /// Record that validation passed; the task is ready for merge
    Pass { task: String },
    /// Mark a task that is ready for merge done
    Done { task: String },
    /// Cancel an open task
    Cancel { task: String },
    /// Attach a commit SHA or URL
    Link {
        task: String,
        #[command(flatten)]
        target: LinkTarget,
    },
}

/// Narrow a task query to one project and/or one goal.
#[derive(Args)]
struct Scope {
    #[arg(long)]
    project: Option<String>,
    #[arg(long)]
    goal: Option<String>,
}

impl From<Scope> for TaskScope {
    fn from(scope: Scope) -> Self {
        Self {
            project: scope.project,
            goal: scope.goal,
        }
    }
}

#[derive(Args)]
#[group(required = true, multiple = false)]
struct LinkTarget {
    #[arg(long)]
    commit: Option<String>,
    #[arg(long)]
    url: Option<String>,
}

/// Optional body given at creation time.
#[derive(Args)]
#[group(multiple = false)]
struct BodySource {
    /// What to build, given inline
    #[arg(long)]
    body: Option<String>,
    /// Read the body from a file
    #[arg(long)]
    body_file: Option<PathBuf>,
}

impl BodySource {
    fn read(self) -> Result<String> {
        if let Some(text) = self.body {
            return Ok(text);
        }
        self.body_file
            .as_deref()
            .map(read_file)
            .transpose()
            .map(Option::unwrap_or_default)
    }
}

/// Optional test plan given at creation time.
#[derive(Args)]
#[group(multiple = false)]
struct TestPlanSource {
    /// Validation steps given inline
    #[arg(long)]
    test_plan: Option<String>,
    /// Read the validation steps from a file
    #[arg(long)]
    test_plan_file: Option<PathBuf>,
}

impl TestPlanSource {
    fn read(self) -> Result<String> {
        if let Some(text) = self.test_plan {
            return Ok(text);
        }
        self.test_plan_file
            .as_deref()
            .map(read_file)
            .transpose()
            .map(Option::unwrap_or_default)
    }
}

fn read_file(path: &Path) -> Result<String> {
    std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))
}

impl SpecSource {
    fn read(self) -> Result<Option<String>> {
        if let Some(text) = self.spec {
            return Ok(Some(text));
        }
        self.spec_file.as_deref().map(read_file).transpose()
    }
}

impl TextSource {
    fn read(self) -> Result<String> {
        if let Some(text) = self.text {
            return Ok(text);
        }
        if let Some(path) = self.file {
            return read_file(&path);
        }
        let mut body = String::new();
        io::stdin()
            .read_to_string(&mut body)
            .context("reading text from stdin")?;
        Ok(body)
    }
}

impl LinkTarget {
    fn into_parts(self) -> (LinkKind, String) {
        if let Some(commit) = self.commit {
            (LinkKind::Commit, commit)
        } else {
            (LinkKind::Url, self.url.unwrap_or_default())
        }
    }
}

/// Run a command. `None` means there is nothing to print, as after the viewer closes.
fn execute(db: &Path, command: Command) -> Result<Option<Value>> {
    match command {
        Command::Init => {
            Store::init(db)?;
            return Ok(Some(json!({ "created": db })));
        }
        Command::Migrate => return Ok(Some(json!({ "applied": Store::migrate(db)? }))),
        Command::Ui => {
            tasky_ui::run(db)?;
            return Ok(None);
        }
        Command::Reference => {
            print!("{}", reference());
            return Ok(None);
        }
        _ => {}
    }
    let mut store = Store::open(db)?;
    Ok(Some(match command {
        Command::Init | Command::Migrate | Command::Ui | Command::Reference => {
            unreachable!("handled above")
        }
        Command::Project(command) => run_project(&mut store, command)?,
        Command::Goal(command) => run_goal(&mut store, command)?,
        Command::Task(command) => run_task(&mut store, command)?,
    }))
}

fn run_project(store: &mut Store, command: ProjectCommand) -> Result<Value> {
    Ok(match command {
        ProjectCommand::Add {
            slug,
            name,
            parent,
            repo_path,
            repo_url,
        } => {
            let name = name.unwrap_or_else(|| slug.clone());
            json!(store.create_project(parent.as_deref(), slug, name, repo_path, repo_url)?)
        }
        ProjectCommand::List => json!(store.projects()?),
        ProjectCommand::Repo { project, change } => {
            let (path, url) = change.into_parts();
            json!(store.set_project_repo(&project, path, url)?)
        }
        ProjectCommand::Show { project } => json!(store.project_detail(&project)?),
    })
}

fn run_goal(store: &mut Store, command: GoalCommand) -> Result<Value> {
    Ok(match command {
        GoalCommand::Add {
            project,
            slug,
            title,
            parent,
            description,
            spec,
        } => json!(store.create_goal(
            &project,
            parent.as_deref(),
            slug,
            title,
            description,
            spec.read()?
        )?),
        GoalCommand::List { project } => json!(store.goals(project.as_deref())?),
        GoalCommand::Show { goal } => json!(store.goal_detail(&goal)?),
        GoalCommand::Spec {
            goal,
            source,
            clear,
        } => {
            let spec = if clear { None } else { Some(source.read()?) };
            json!(store.set_goal_spec(&goal, spec)?)
        }
        GoalCommand::Activate { goal } => json!(store.activate_goal(&goal)?),
        GoalCommand::Complete { goal } => json!(store.complete_goal(&goal)?),
        GoalCommand::Cancel { goal } => json!(store.cancel_goal(&goal)?),
    })
}

fn run_task(store: &mut Store, command: TaskCommand) -> Result<Value> {
    Ok(match command {
        TaskCommand::Add {
            goal,
            title,
            body,
            test_plan,
        } => json!(store.create_task(&goal, title, body.read()?, test_plan.read()?)?),
        TaskCommand::Title { task, title } => json!(store.set_title(&task, title)?),
        TaskCommand::Body { task, source } => json!(store.set_body(&task, source.read()?)?),
        TaskCommand::TestPlan { task, source } => {
            json!(store.set_test_plan(&task, source.read()?)?)
        }
        TaskCommand::Pr { task, pr, clear } => {
            json!(store.set_pr(&task, if clear { None } else { pr })?)
        }
        TaskCommand::List { scope, status } => json!(store.tasks(&TaskFilter {
            scope: scope.into(),
            status,
        })?),
        TaskCommand::Show { task } => json!(store.task_detail(&task)?),
        TaskCommand::Ready { scope } => json!(store.ready_tasks(&scope.into())?),
        TaskCommand::Order { scope } => json!(store.task_order(&scope.into())?),
        TaskCommand::Depend { task, depends_on } => {
            json!(store.add_dependency(&task, &depends_on)?)
        }
        TaskCommand::Undepend { task, depends_on } => {
            store.remove_dependency(&task, &depends_on)?;
            json!({"removed": true})
        }
        TaskCommand::Start { task } => json!(store.start_task(&task)?),
        TaskCommand::Test { task } => json!(store.test_task(&task)?),
        TaskCommand::Fail { task } => json!(store.fail_task(&task)?),
        TaskCommand::Pass { task } => json!(store.pass_task(&task)?),
        TaskCommand::Done { task } => json!(store.finish_task(&task)?),
        TaskCommand::Cancel { task } => json!(store.cancel_task(&task)?),
        TaskCommand::Link { task, target } => {
            let (kind, reference) = target.into_parts();
            json!(store.add_link(&task, kind, reference)?)
        }
    })
}

/// Every command's long help, depth first, as one Markdown document.
fn reference() -> String {
    fn walk(command: &mut clap::Command, path: &str, out: &mut String) {
        if command.is_hide_set() || command.get_name() == "help" {
            return;
        }
        let full = if path.is_empty() {
            command.get_name().to_owned()
        } else {
            format!("{path} {}", command.get_name())
        };
        let help = command.render_long_help().to_string();
        out.push_str("## `");
        out.push_str(&full);
        out.push_str("`\n\n```text\n");
        out.push_str(help.trim_end());
        out.push_str("\n```\n\n");
        let children: Vec<String> = command
            .get_subcommands()
            .map(|sub| sub.get_name().to_owned())
            .collect();
        for name in children {
            if let Some(sub) = command.find_subcommand_mut(&name) {
                walk(sub, &full, out);
            }
        }
    }
    let mut out = String::from(
        "# Tasky command reference\n\nGenerated by `tasky reference`; do not edit by hand. \
         Regenerate with `scripts/update-skill-reference.sh` after changing the CLI.\n\n",
    );
    let mut root = Cli::command();
    root.build();
    walk(&mut root, "", &mut out);
    // The default database path is machine-specific; keep the reference portable.
    out.replace(
        &default_path().display().to_string(),
        "$XDG_DATA_HOME/tasky/tasky.db",
    )
}

fn main() {
    let cli = Cli::parse();
    let json = cli.json;
    let result = execute(&cli.db, cli.command).and_then(|value| {
        let Some(value) = value else {
            return Ok(());
        };
        let mut stdout = io::stdout().lock();
        if json {
            serde_json::to_writer(&mut stdout, &value)?;
        } else {
            serde_json::to_writer_pretty(&mut stdout, &value)?;
        }
        writeln!(stdout)?;
        Ok(())
    });
    if let Err(error) = result {
        if json {
            eprintln!("{}", json!({"error": {"message": format!("{error:#}")}}));
        } else {
            eprintln!("error: {error:#}");
        }
        std::process::exit(1);
    }
}
