//! Pure layout of projects, goals, and tasks as one left-to-right graph in world units.
//!
//! Nothing here touches GPUI, so it is unit-tested without a display. A root project sits
//! in column 0; its goals one column right; a goal's tasks further right by dependency rank.
//! Sub-projects and sub-goals indent one column to the right beneath their parent, at any
//! depth, so every hierarchy edge flows left to right. Shapes are large because a node's
//! title lives inside it and appears as the viewer zooms in.
//!
//! A goal is joined only to its tasks that have no prerequisite inside the same goal; every
//! other task is already reached from the goal through dependency edges, so drawing the
//! hierarchy edge as well would only add clutter.
//!
//! Projects and goals can be collapsed, which hides everything beneath them.

use std::collections::{BTreeMap, HashMap, HashSet};
use tasky_core::{Goal, GoalStatus, Project, TaskStatus};
use tasky_store::TaskDetail;

/// Half extents of each shape: projects are wide diamonds, goals wide rectangles, tasks circles.
pub(crate) const PROJECT_HW: f32 = 150.0;
pub(crate) const PROJECT_HH: f32 = 90.0;
pub(crate) const GOAL_HW: f32 = 120.0;
pub(crate) const GOAL_HH: f32 = 70.0;
pub(crate) const TASK_R: f32 = 64.0;
const COL_GAP: f32 = 130.0;
/// Horizontal offset from a project's centre to its goals' centre, and to a sub-project's.
const GOAL_OFFSET: f32 = PROJECT_HW + COL_GAP + GOAL_HW;
const SUBPROJECT_OFFSET: f32 = 2.0 * PROJECT_HW + COL_GAP;
/// Horizontal offset from a goal's centre to its first task column, and to a sub-goal's.
const TASK_OFFSET: f32 = GOAL_HW + COL_GAP + TASK_R;
const SUBGOAL_OFFSET: f32 = 2.0 * GOAL_HW + COL_GAP;
const COL_STEP: f32 = 2.0 * TASK_R + COL_GAP;
const ROW: f32 = 2.0 * TASK_R + 40.0;
const GOAL_GAP: f32 = 48.0;
const PROJECT_GAP: f32 = 160.0;

/// An axis-aligned rectangle in world units.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Project,
    Goal,
    Task,
}

/// Where a goal or task is in its life, merged into one vocabulary so the same meaning
/// always gets the same fill. Projects have no lifecycle and count as `Open`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    /// Not started and free to pick up.
    Open,
    /// Not started because a prerequisite is unfinished.
    Blocked,
    InWork,
    Validating,
    AwaitingMerge,
    Finished,
    Abandoned,
}

impl From<GoalStatus> for State {
    fn from(status: GoalStatus) -> Self {
        match status {
            GoalStatus::Draft => Self::Open,
            GoalStatus::Active => Self::InWork,
            GoalStatus::Complete => Self::Finished,
            GoalStatus::Cancelled => Self::Abandoned,
        }
    }
}

impl State {
    /// A task's state, splitting todo into ready and blocked.
    pub(crate) fn of_task(task: &TaskDetail) -> Self {
        match task.task.status {
            TaskStatus::Todo if task.ready => Self::Open,
            TaskStatus::Todo => Self::Blocked,
            TaskStatus::InProgress => Self::InWork,
            TaskStatus::Testing => Self::Validating,
            TaskStatus::ReadyForMerge => Self::AwaitingMerge,
            TaskStatus::Done => Self::Finished,
            TaskStatus::Cancelled => Self::Abandoned,
        }
    }
}

/// A shape showing its title, centred at (x, y) with half extents (hw, hh). Projects are
/// diamonds, goals rectangles, tasks circles. `collapsed` marks hidden children.
#[derive(Debug, Clone)]
pub(crate) struct Node {
    pub id: String,
    pub kind: Kind,
    pub state: State,
    pub collapsed: bool,
    pub x: f32,
    pub y: f32,
    pub hw: f32,
    pub hh: f32,
    pub title: String,
}

impl Node {
    pub(crate) fn contains(&self, x: f32, y: f32) -> bool {
        let (dx, dy) = ((x - self.x) / self.hw, (y - self.y) / self.hh);
        match self.kind {
            Kind::Project => dx.abs() + dy.abs() <= 1.0,
            Kind::Goal => dx.abs() <= 1.0 && dy.abs() <= 1.0,
            Kind::Task => dx * dx + dy * dy <= 1.0,
        }
    }
}

/// A directed edge between node indices. `dependency` edges join tasks; the rest are hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Edge {
    pub from: usize,
    pub to: usize,
    pub dependency: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Layout {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    pub bounds: Rect,
}

/// Everything the layout needs. Tasks must be in dependency order, prerequisites first.
pub(crate) struct Input<'a> {
    pub projects: &'a [Project],
    pub goals: &'a [Goal],
    pub tasks: &'a [TaskDetail],
    /// IDs of projects and goals whose children are hidden.
    pub collapsed: &'a HashSet<String>,
}

/// What starts collapsed: complete goals, and projects whose goals are all complete and
/// whose sub-projects would all start collapsed themselves.
pub(crate) fn default_collapsed(projects: &[Project], goals: &[Goal]) -> HashSet<String> {
    let mut collapsed: HashSet<String> = goals
        .iter()
        .filter(|goal| goal.status == GoalStatus::Complete)
        .map(|goal| goal.id.clone())
        .collect();
    for project in projects {
        if project_finished(project, projects, goals) {
            collapsed.insert(project.id.clone());
        }
    }
    collapsed
}

/// A project with something beneath it, all of it complete: its own goals and, recursively,
/// its sub-projects.
fn project_finished(project: &Project, projects: &[Project], goals: &[Goal]) -> bool {
    let own: Vec<&Goal> = goals
        .iter()
        .filter(|goal| goal.project_id == project.id)
        .collect();
    let children: Vec<&Project> = projects
        .iter()
        .filter(|p| p.parent_id.as_deref() == Some(project.id.as_str()))
        .collect();
    (!own.is_empty() || !children.is_empty())
        && own.iter().all(|goal| goal.status == GoalStatus::Complete)
        && children
            .iter()
            .all(|child| project_finished(child, projects, goals))
}

fn count(n: usize) -> f32 {
    f32::from(u16::try_from(n).unwrap_or(u16::MAX))
}

/// Dependency rank per task: longest prerequisite chain, so dependents sit right of prerequisites.
fn ranks(tasks: &[TaskDetail]) -> HashMap<&str, usize> {
    let mut ranks: HashMap<&str, usize> = HashMap::new();
    for task in tasks {
        let rank = task
            .depends_on
            .iter()
            .filter_map(|dep| ranks.get(dep.as_str()))
            .max()
            .map_or(0, |max| max + 1);
        ranks.insert(&task.task.id, rank);
    }
    ranks
}

struct Builder<'a> {
    input: &'a Input<'a>,
    ranks: HashMap<&'a str, usize>,
    /// Task ID → goal ID, for deciding which tasks the goal itself must point at.
    goal_of: HashMap<&'a str, &'a str>,
    task_index: HashMap<&'a str, usize>,
    out: Layout,
}

impl<'a> Builder<'a> {
    fn push(&mut self, id: &str, kind: Kind, state: State, x: f32, y: f32, title: &str) -> usize {
        let (hw, hh) = match kind {
            Kind::Project => (PROJECT_HW, PROJECT_HH),
            Kind::Goal => (GOAL_HW, GOAL_HH),
            Kind::Task => (TASK_R, TASK_R),
        };
        self.out.nodes.push(Node {
            id: id.to_owned(),
            kind,
            state,
            collapsed: self.input.collapsed.contains(id),
            x,
            y,
            hw,
            hh,
            title: title.to_owned(),
        });
        self.out.nodes.len() - 1
    }

    /// Place a goal's direct tasks in rows by dependency rank, starting at `top`, with the
    /// goal centred at `x`; returns the rows' height (zero when the goal has no tasks).
    fn task_rows(&mut self, goal: &'a Goal, goal_node: usize, x: f32, top: f32) -> f32 {
        let mut rows_in_column: BTreeMap<usize, usize> = BTreeMap::new();
        let mut placed = Vec::new();
        for task in self
            .input
            .tasks
            .iter()
            .filter(|task| task.task.goal_id == goal.id)
        {
            let column = self.ranks[task.task.id.as_str()];
            let row = rows_in_column.entry(column).or_default();
            placed.push((task, column, *row));
            *row += 1;
        }
        let rows = rows_in_column.values().copied().max().unwrap_or(0);
        for (task, column, row) in placed {
            let tx = x + TASK_OFFSET + count(column) * COL_STEP;
            let ty = top + count(row) * ROW + ROW / 2.0;
            let node = self.push(
                &task.task.id,
                Kind::Task,
                State::of_task(task),
                tx,
                ty,
                &task.task.title,
            );
            self.task_index.insert(&task.task.id, node);
            let rooted_in_goal = task
                .depends_on
                .iter()
                .any(|dep| self.goal_of.get(dep.as_str()) == Some(&goal.id.as_str()));
            if !rooted_in_goal {
                self.out.edges.push(Edge {
                    from: goal_node,
                    to: node,
                    dependency: false,
                });
            }
        }
        count(rows) * ROW
    }

    /// Place a goal centred at `x`, and unless collapsed its tasks and then its sub-goals
    /// one column to the right, starting at `top`; returns the height used.
    fn goal_block(&mut self, goal: &'a Goal, x: f32, top: f32) -> f32 {
        let node = self.push(
            &goal.id,
            Kind::Goal,
            goal.status.into(),
            x,
            0.0,
            &goal.title,
        );
        let mut y = top;
        if !self.input.collapsed.contains(&goal.id) {
            y += self.task_rows(goal, node, x, y);
            let subgoals: Vec<&'a Goal> = self
                .input
                .goals
                .iter()
                .filter(|g| g.parent_id.as_deref() == Some(goal.id.as_str()))
                .collect();
            for sub in subgoals {
                if y > top {
                    y += GOAL_GAP;
                }
                let child = self.out.nodes.len();
                y += self.goal_block(sub, x + SUBGOAL_OFFSET, y);
                self.out.edges.push(Edge {
                    from: node,
                    to: child,
                    dependency: false,
                });
            }
        }
        let height = (y - top).max(2.0 * GOAL_HH);
        self.out.nodes[node].y = top + height / 2.0;
        height
    }

    /// Place a project centred at `x`, and unless collapsed its goals and then its
    /// sub-projects one column to the right, starting at `top`; returns the height used.
    fn project_block(&mut self, project: &'a Project, x: f32, top: f32) -> f32 {
        let node = self.push(
            &project.id,
            Kind::Project,
            State::Open,
            x,
            0.0,
            &project.name,
        );
        let mut y = top;
        if !self.input.collapsed.contains(&project.id) {
            let goals: Vec<&'a Goal> = self
                .input
                .goals
                .iter()
                .filter(|goal| goal.project_id == project.id && goal.parent_id.is_none())
                .collect();
            let children: Vec<&'a Project> = self
                .input
                .projects
                .iter()
                .filter(|p| p.parent_id.as_deref() == Some(project.id.as_str()))
                .collect();
            for goal in goals {
                if y > top {
                    y += GOAL_GAP;
                }
                let child = self.out.nodes.len();
                y += self.goal_block(goal, x + GOAL_OFFSET, y);
                self.out.edges.push(Edge {
                    from: node,
                    to: child,
                    dependency: false,
                });
            }
            for sub in children {
                if y > top {
                    y += GOAL_GAP;
                }
                let child = self.out.nodes.len();
                y += self.project_block(sub, x + SUBPROJECT_OFFSET, y);
                self.out.edges.push(Edge {
                    from: node,
                    to: child,
                    dependency: false,
                });
            }
        }
        let height = (y - top).max(2.0 * PROJECT_HH);
        self.out.nodes[node].y = top + height / 2.0;
        height
    }

    /// Dependency edges between visible tasks only.
    fn dependencies(&mut self) {
        for task in self.input.tasks {
            let Some(&to) = self.task_index.get(task.task.id.as_str()) else {
                continue;
            };
            for dep in &task.depends_on {
                if let Some(&from) = self.task_index.get(dep.as_str()) {
                    self.out.edges.push(Edge {
                        from,
                        to,
                        dependency: true,
                    });
                }
            }
        }
    }

    fn bounds(&mut self) {
        let mut min = (f32::MAX, f32::MAX);
        let mut max = (f32::MIN, f32::MIN);
        for node in &self.out.nodes {
            min = (min.0.min(node.x - node.hw), min.1.min(node.y - node.hh));
            max = (max.0.max(node.x + node.hw), max.1.max(node.y + node.hh));
        }
        self.out.bounds = if self.out.nodes.is_empty() {
            Rect::default()
        } else {
            Rect {
                x: min.0,
                y: min.1,
                w: max.0 - min.0,
                h: max.1 - min.1,
            }
        };
    }
}

/// Lay out every visible project, goal, and task, roots first. Deterministic for a given
/// input.
pub(crate) fn layout(input: &Input<'_>) -> Layout {
    let mut builder = Builder {
        input,
        ranks: ranks(input.tasks),
        goal_of: input
            .tasks
            .iter()
            .map(|task| (task.task.id.as_str(), task.task.goal_id.as_str()))
            .collect(),
        task_index: HashMap::new(),
        out: Layout::default(),
    };
    let mut y = 0.0;
    for project in input.projects.iter().filter(|p| p.parent_id.is_none()) {
        y += builder.project_block(project, 0.0, y) + PROJECT_GAP;
    }
    builder.dependencies();
    builder.bounds();
    builder.out
}

#[cfg(test)]
mod tests {
    use super::*;
    use tasky_core::{Task, Timestamp};

    fn project(id: &str) -> Project {
        project_in(id, None)
    }

    fn project_in(id: &str, parent: Option<&str>) -> Project {
        Project::new(
            id.into(),
            parent.map(str::to_owned),
            id.into(),
            id.to_uppercase(),
            None,
            None,
            Timestamp::UNIX_EPOCH,
        )
        .unwrap()
    }

    fn goal(id: &str, project: &str) -> Goal {
        goal_in(id, project, None)
    }

    fn goal_in(id: &str, project: &str, parent: Option<&str>) -> Goal {
        Goal::new(
            id.into(),
            project.into(),
            parent.map(str::to_owned),
            id.into(),
            id.to_uppercase(),
            String::new(),
            None,
            Timestamp::UNIX_EPOCH,
        )
        .unwrap()
    }

    fn complete_goal(id: &str, project: &str) -> Goal {
        let mut goal = goal(id, project);
        goal.status = GoalStatus::Complete;
        goal
    }

    fn task(id: &str, goal: &str, deps: &[&str], done: bool) -> TaskDetail {
        let mut task = Task::new(
            id.into(),
            goal.into(),
            id.to_uppercase(),
            String::new(),
            String::new(),
            Timestamp::UNIX_EPOCH,
        )
        .unwrap();
        if done {
            task.status = TaskStatus::Done;
        }
        TaskDetail {
            task,
            project: "p".into(),
            goal: goal.into(),
            ready: deps.is_empty() && !done,
            depends_on: deps.iter().map(|d| (*d).to_owned()).collect(),
            blocked_by: Vec::new(),
            dependents: Vec::new(),
            links: Vec::new(),
        }
    }

    fn node<'a>(layout: &'a Layout, title: &str) -> &'a Node {
        layout
            .nodes
            .iter()
            .find(|node| node.title == title)
            .unwrap()
    }

    fn build(projects: &[Project], goals: &[Goal], tasks: &[TaskDetail]) -> Layout {
        build_with(projects, goals, tasks, &HashSet::new())
    }

    fn build_with(
        projects: &[Project],
        goals: &[Goal],
        tasks: &[TaskDetail],
        collapsed: &HashSet<String>,
    ) -> Layout {
        layout(&Input {
            projects,
            goals,
            tasks,
            collapsed,
        })
    }

    #[test]
    fn columns_follow_hierarchy_and_dependency_rank() {
        let layout = build(
            &[project("p")],
            &[goal("g1", "p"), goal("g2", "p")],
            &[
                task("a", "g1", &[], true),
                task("b", "g1", &["a"], false),
                task("c", "g2", &["b"], false),
                task("d", "g2", &[], false),
            ],
        );
        let (p, g1, g2) = (node(&layout, "P"), node(&layout, "G1"), node(&layout, "G2"));
        let (ta, tb, tc, td) = (
            node(&layout, "A"),
            node(&layout, "B"),
            node(&layout, "C"),
            node(&layout, "D"),
        );
        assert!(p.x < g1.x && g1.x < ta.x, "project, goal, task columns");
        assert!((g1.x - g2.x).abs() < 0.01);
        assert!(ta.x < tb.x && tb.x < tc.x, "dependents move right");
        assert!(
            (ta.x - td.x).abs() < 0.01,
            "independent tasks share column 0"
        );
        assert!(tc.y > tb.y, "second goal's band is below the first");
        assert!(g1.y < g2.y);
        assert!(p.y > g1.y && p.y < g2.y, "project centred on its goals");
        assert_eq!(p.kind, Kind::Project);
        assert!((ta.hw - TASK_R).abs() < 0.01 && (ta.hh - TASK_R).abs() < 0.01);
        assert!(tb.x - ta.x >= 2.0 * TASK_R, "circles never overlap");
        assert!(g1.x - p.x >= PROJECT_HW + GOAL_HW, "shapes never overlap");
        assert_eq!(ta.state, State::Finished);
        assert_eq!(
            tb.state,
            State::Blocked,
            "b waits on a, which the fixture marks not ready"
        );
        assert_eq!(td.state, State::Open, "d has no prerequisites");
        let mut testing = task("t", "g1", &[], false);
        testing.task.status = TaskStatus::Testing;
        assert_eq!(State::of_task(&testing), State::Validating);
        assert!(ta.contains(ta.x + 1.0, ta.y - 1.0));
        assert!(!ta.contains(ta.x + TASK_R + 1.0, ta.y));
        assert!(
            !ta.contains(ta.x + TASK_R * 0.8, ta.y + TASK_R * 0.8),
            "circle corners are outside"
        );
        assert!(
            g1.contains(g1.x + GOAL_HW * 0.9, g1.y + GOAL_HH * 0.9),
            "rectangle corners are inside"
        );
        assert!(p.contains(p.x + PROJECT_HW * 0.4, p.y + PROJECT_HH * 0.4));
        assert!(
            !p.contains(p.x + PROJECT_HW * 0.8, p.y + PROJECT_HH * 0.8),
            "diamond corners are outside"
        );
    }

    #[test]
    fn edges_cover_hierarchy_and_dependencies() {
        let layout = build(
            &[project("p")],
            &[goal("g", "p")],
            &[task("a", "g", &[], false), task("b", "g", &["a"], false)],
        );
        let hierarchy = layout.edges.iter().filter(|e| !e.dependency).count();
        let deps: Vec<&Edge> = layout.edges.iter().filter(|e| e.dependency).collect();
        assert_eq!(
            hierarchy, 2,
            "project→goal and goal→a; b is reached through a"
        );
        assert_eq!(deps.len(), 1);
        assert_eq!(layout.nodes[deps[0].from].title, "A");
        assert_eq!(layout.nodes[deps[0].to].title, "B");
        for edge in &layout.edges {
            assert!(
                layout.nodes[edge.from].x < layout.nodes[edge.to].x,
                "every edge flows left to right"
            );
        }
    }

    #[test]
    fn same_column_tasks_do_not_overlap_and_projects_stack() {
        let layout = build(
            &[project("p1"), project("p2")],
            &[goal("g1", "p1"), goal("g2", "p2")],
            &[task("a", "g1", &[], false), task("b", "g1", &[], false)],
        );
        let (ta, tb) = (node(&layout, "A"), node(&layout, "B"));
        assert!((ta.x - tb.x).abs() < 0.01);
        assert!(tb.y - ta.y >= 2.0 * TASK_R);
        let (p1, p2) = (node(&layout, "P1"), node(&layout, "P2"));
        assert!(p2.y - p1.y >= 2.0 * PROJECT_HH);
        assert!(layout.bounds.w > GOAL_OFFSET && layout.bounds.h > 0.0);
        assert!((layout.bounds.x + PROJECT_HW).abs() < 0.01);
    }

    #[test]
    fn goal_edges_go_only_to_tasks_not_rooted_in_the_goal() {
        // a ← b inside g1; c in g2 depends on b (another goal); d in g2 is independent.
        let layout = build(
            &[project("p")],
            &[goal("g1", "p"), goal("g2", "p")],
            &[
                task("a", "g1", &[], false),
                task("b", "g1", &["a"], false),
                task("c", "g2", &["b"], false),
                task("d", "g2", &[], false),
            ],
        );
        let joined: Vec<(&str, &str)> = layout
            .edges
            .iter()
            .filter(|e| !e.dependency)
            .map(|e| {
                (
                    layout.nodes[e.from].title.as_str(),
                    layout.nodes[e.to].title.as_str(),
                )
            })
            .collect();
        assert!(joined.contains(&("G1", "A")));
        assert!(!joined.contains(&("G1", "B")), "b follows a within g1");
        assert!(
            joined.contains(&("G2", "C")),
            "c's only prerequisite is in another goal"
        );
        assert!(joined.contains(&("G2", "D")));
        assert!(joined.contains(&("P", "G1")) && joined.contains(&("P", "G2")));
    }

    #[test]
    fn collapsing_hides_children_and_their_edges() {
        let projects = [project("p1"), project("p2")];
        let goals = [goal("g1", "p1"), goal("g2", "p1"), goal("g3", "p2")];
        let tasks = [
            task("a", "g1", &[], false),
            task("b", "g1", &["a"], false),
            task("e", "g1", &[], false),
            task("c", "g2", &["b"], false),
            task("d", "g3", &[], false),
        ];
        let open = build(&projects, &goals, &tasks);
        assert_eq!(open.nodes.len(), 10);

        let collapsed: HashSet<String> = ["g1".to_owned()].into_iter().collect();
        let layout = build_with(&projects, &goals, &tasks, &collapsed);
        let titles: Vec<&str> = layout.nodes.iter().map(|n| n.title.as_str()).collect();
        assert!(!titles.contains(&"A") && !titles.contains(&"B") && !titles.contains(&"E"));
        assert!(titles.contains(&"C"), "other goals keep their tasks");
        assert!(node(&layout, "G1").collapsed);
        assert!(!node(&layout, "G2").collapsed);
        assert!(
            layout.edges.iter().all(|e| !e.dependency),
            "c's prerequisite b is hidden, so its dependency edge is gone"
        );
        assert!(layout.bounds.h < open.bounds.h, "collapsing frees space");

        let collapsed: HashSet<String> = ["p1".to_owned()].into_iter().collect();
        let layout = build_with(&projects, &goals, &tasks, &collapsed);
        let titles: Vec<&str> = layout.nodes.iter().map(|n| n.title.as_str()).collect();
        assert_eq!(titles, ["P1", "P2", "G3", "D"]);
        assert!(node(&layout, "P1").collapsed);
        let p1 = node(&layout, "P1");
        assert!(p1.contains(p1.x, p1.y));
    }

    #[test]
    fn nested_projects_and_goals_indent_beneath_their_parents() {
        let projects = [project("p"), project_in("sub", Some("p"))];
        let goals = [
            goal("g", "p"),
            goal_in("child", "p", Some("g")),
            goal("sg", "sub"),
        ];
        let tasks = [
            task("a", "g", &[], false),
            task("b", "child", &["a"], false),
            task("c", "sg", &[], false),
        ];
        let layout = build(&projects, &goals, &tasks);
        let (p, sub) = (node(&layout, "P"), node(&layout, "SUB"));
        let (g, child, sg) = (
            node(&layout, "G"),
            node(&layout, "CHILD"),
            node(&layout, "SG"),
        );
        assert!(sub.x > p.x, "sub-project indents right");
        assert!(sub.y > g.y, "sub-project sits below the parent's goals");
        assert!(child.x > g.x, "sub-goal indents right of its parent goal");
        assert!(
            child.y > node(&layout, "A").y,
            "sub-goal sits below the parent's tasks"
        );
        assert!(
            sg.x > sub.x,
            "a sub-project's goal is right of the sub-project"
        );
        assert!(node(&layout, "B").x > child.x && node(&layout, "C").x > sg.x);
        let joined: Vec<(&str, &str)> = layout
            .edges
            .iter()
            .filter(|e| !e.dependency)
            .map(|e| {
                (
                    layout.nodes[e.from].title.as_str(),
                    layout.nodes[e.to].title.as_str(),
                )
            })
            .collect();
        assert!(joined.contains(&("P", "SUB")));
        assert!(joined.contains(&("G", "CHILD")));
        assert!(joined.contains(&("SUB", "SG")));
        assert!(
            !joined.contains(&("P", "CHILD")),
            "a sub-goal hangs off its goal, not the project"
        );
        assert!(
            p.y > g.y && p.y < sub.y,
            "project centred on its whole block"
        );

        let collapsed: HashSet<String> = ["g".to_owned()].into_iter().collect();
        let layout = build_with(&projects, &goals, &tasks, &collapsed);
        let titles: Vec<&str> = layout.nodes.iter().map(|n| n.title.as_str()).collect();
        assert!(
            !titles.contains(&"CHILD") && !titles.contains(&"B"),
            "subtree hidden"
        );
        assert!(titles.contains(&"SUB") && titles.contains(&"C"));
        let collapsed: HashSet<String> = ["p".to_owned()].into_iter().collect();
        let layout = build_with(&projects, &goals, &tasks, &collapsed);
        assert_eq!(
            layout.nodes.len(),
            1,
            "collapsing a project hides sub-projects too"
        );
    }

    #[test]
    fn complete_goals_and_fully_complete_projects_start_collapsed() {
        let projects = [project("done"), project("mixed"), project("empty")];
        let goals = [
            complete_goal("g1", "done"),
            complete_goal("g2", "done"),
            complete_goal("g3", "mixed"),
            goal("g4", "mixed"),
        ];
        let collapsed = default_collapsed(&projects, &goals);
        assert!(collapsed.contains("g1") && collapsed.contains("g2"));
        assert!(collapsed.contains("g3"));
        assert!(!collapsed.contains("g4"));
        assert!(collapsed.contains("done"), "every goal complete");
        assert!(!collapsed.contains("mixed"));
        assert!(!collapsed.contains("empty"), "nothing to collapse");

        let projects = [project("root"), project_in("leaf", Some("root"))];
        let goals = [complete_goal("lg", "leaf")];
        let collapsed = default_collapsed(&projects, &goals);
        assert!(collapsed.contains("leaf"));
        assert!(
            collapsed.contains("root"),
            "all its sub-projects are finished"
        );
        let goals = [complete_goal("lg", "leaf"), goal("open", "root")];
        assert!(!default_collapsed(&projects, &goals).contains("root"));
    }
}
