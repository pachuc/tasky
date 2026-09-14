//! Read-only GPUI viewer: a pannable, zoomable graph of every project, goal, and task,
//! launched from the CLI with `tasky ui`.

mod layout;

use gpui::{
    App, Application, AssetSource, BorderStyle, Bounds, ClickEvent, ContentMask, Context,
    CursorStyle, Div, FontWeight, Hsla, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Path, Pixels, Point, ScrollDelta, ScrollWheelEvent, SharedString, Size, Stateful, TextAlign,
    TextRun, Window, WindowBounds, WindowOptions, WrappedLine, black, canvas, div, fill, point,
    prelude::*, px, quad, rgb, size, svg, transparent_black, white,
};
use layout::{Kind, Layout, Node, Rect, State};
use std::{
    borrow::Cow,
    cell::Cell,
    collections::{BTreeMap, HashSet},
    rc::Rc,
    time::Instant,
};
use tasky_core::{Goal, Project, TaskStatus};
use tasky_store::{Store, TaskDetail, TaskScope};

const MIN_SCALE: f32 = 0.04;
const MAX_SCALE: f32 = 4.0;

/// World → screen transform: `screen = graph_origin + world * scale + (x, y)`.
#[derive(Debug, Clone, Copy)]
struct Camera {
    x: f32,
    y: f32,
    scale: f32,
}

impl Camera {
    fn fit(bounds: Rect, viewport: Size<Pixels>) -> Self {
        let (vw, vh) = (f32::from(viewport.width), f32::from(viewport.height));
        let scale = (vw / bounds.w.max(1.0))
            .min(vh / bounds.h.max(1.0))
            .clamp(MIN_SCALE, 1.0)
            * 0.94;
        Self {
            x: (vw - bounds.w * scale) / 2.0 - bounds.x * scale,
            y: (vh - bounds.h * scale) / 2.0 - bounds.y * scale,
            scale,
        }
    }

    /// Zoom so the world point under `local` stays under the cursor.
    fn zoom_at(&mut self, local: Point<f32>, factor: f32) {
        let scale = (self.scale * factor).clamp(MIN_SCALE, MAX_SCALE);
        let ratio = scale / self.scale;
        self.x = local.x - (local.x - self.x) * ratio;
        self.y = local.y - (local.y - self.y) * ratio;
        self.scale = scale;
    }
}

/// State shared between the entity and the canvas closures, which cannot borrow the entity.
#[derive(Debug, Clone, Copy)]
struct Shared {
    camera: Camera,
    origin: Point<Pixels>,
    fit_pending: bool,
}

/// Everything loaded from the store, kept so the graph can be re-laid out on collapse.
struct Snapshot {
    projects: Vec<Project>,
    goals: Vec<Goal>,
    /// Dependency order, prerequisites first.
    tasks: Vec<TaskDetail>,
}

impl Snapshot {
    fn load(store: &mut Store) -> anyhow::Result<Self> {
        let projects = store.projects()?;
        let goals = store.goals(None)?;
        let tasks = store
            .task_order(&TaskScope::default())?
            .iter()
            .map(|task| store.task_detail(&task.id))
            .collect::<anyhow::Result<Vec<_>>>()?;
        Ok(Self {
            projects,
            goals,
            tasks,
        })
    }

    fn layout(&self, collapsed: &HashSet<String>) -> Layout {
        layout::layout(&layout::Input {
            projects: &self.projects,
            goals: &self.goals,
            tasks: &self.tasks,
            collapsed,
        })
    }
}

/// A pressed mouse button: where it went down and whether it has moved enough to be a drag.
#[derive(Debug, Clone, Copy)]
struct Press {
    last: Point<Pixels>,
    dragged: bool,
}

struct Viewer {
    snapshot: Snapshot,
    collapsed: HashSet<String>,
    layout: Rc<Layout>,
    shared: Rc<Cell<Shared>>,
    press: Option<Press>,
    /// ID of the node whose details are shown in the side panel.
    selected: Option<String>,
    /// Whether the key explaining shapes and colours is open.
    legend_open: bool,
    /// When the viewer started; drives the pulse on nodes being worked on.
    started: Instant,
}

impl Viewer {
    fn new(snapshot: Snapshot) -> Self {
        let collapsed = layout::default_collapsed(&snapshot.projects, &snapshot.goals);
        let layout = Rc::new(snapshot.layout(&collapsed));
        Self {
            snapshot,
            collapsed,
            layout,
            shared: Rc::new(Cell::new(Shared {
                camera: Camera {
                    x: 0.0,
                    y: 0.0,
                    scale: 1.0,
                },
                origin: Point::default(),
                fit_pending: true,
            })),
            press: None,
            selected: None,
            legend_open: false,
            started: Instant::now(),
        }
    }

    fn update_shared(&self, apply: impl FnOnce(&mut Shared)) {
        let mut shared = self.shared.get();
        apply(&mut shared);
        self.shared.set(shared);
    }

    fn local(&self, position: Point<Pixels>) -> Point<f32> {
        let origin = self.shared.get().origin;
        point(
            f32::from(position.x - origin.x),
            f32::from(position.y - origin.y),
        )
    }

    fn zoom_at(&self, position: Point<Pixels>, factor: f32) {
        let local = self.local(position);
        self.update_shared(|shared| shared.camera.zoom_at(local, factor));
    }

    /// The node under a window position, if any.
    fn node_at(&self, position: Point<Pixels>) -> Option<&Node> {
        let local = self.local(position);
        let camera = self.shared.get().camera;
        let (x, y) = (
            (local.x - camera.x) / camera.scale,
            (local.y - camera.y) / camera.scale,
        );
        self.layout.nodes.iter().find(|node| node.contains(x, y))
    }

    /// Single click: show the node's details in the side panel, or close the panel when
    /// the node is the one already shown.
    fn select_at(&mut self, position: Point<Pixels>) {
        let Some(id) = self.node_at(position).map(|node| node.id.clone()) else {
            return;
        };
        if self.selected.as_deref() == Some(id.as_str()) {
            self.selected = None;
        } else {
            self.selected = Some(id);
        }
    }

    /// Right click: toggle a project or goal and re-lay out the graph. Tasks never
    /// collapse: what follows a task is a dependency chain, not its children.
    fn toggle_at(&mut self, position: Point<Pixels>) {
        let hit = self
            .node_at(position)
            .filter(|node| node.kind != Kind::Task)
            .map(|node| node.id.clone());
        let Some(id) = hit else {
            return;
        };
        if !self.collapsed.remove(&id) {
            self.collapsed.insert(id);
        }
        self.layout = Rc::new(self.snapshot.layout(&self.collapsed));
    }
}

// ---------------------------------------------------------------------------------------------
// Painting

/// Text only renders once its on-screen size is readable.
const MIN_FONT_PX: f32 = 7.0;

/// Shape fill by state, fully saturated; ready nodes are white and blocked ones grey.
fn fill_color(state: State) -> Hsla {
    match state {
        State::Open => white(),
        State::Blocked => rgb(0x00b4_b4b4).into(),
        State::InWork => rgb(0x00fa_cc15).into(),
        State::Validating => rgb(0x002d_d4bf).into(),
        State::AwaitingMerge => rgb(0x0086_efac).into(),
        State::Finished => rgb(0x0022_c55e).into(),
        State::Abandoned => rgb(0x00ef_4444).into(),
    }
}

/// Outline width by kind in world units; projects heaviest, tasks lightest. Edges do not
/// follow it; see [`EDGE_WIDTH`].
fn ring_width(kind: Kind) -> f32 {
    match kind {
        Kind::Project => 4.2,
        Kind::Goal => 3.2,
        Kind::Task => 2.4,
    }
}

/// On-screen stroke width for a kind at the current zoom, never thinner than one pixel.
fn screen_width(kind: Kind, scale: f32) -> f32 {
    (ring_width(kind) * scale).clamp(1.0, 7.0)
}

/// Every edge, hierarchy or dependency, shares one stroke width in world units.
const EDGE_WIDTH: f32 = 2.4;

fn edge_width(scale: f32) -> f32 {
    (EDGE_WIDTH * scale).clamp(1.0, 7.0)
}

/// Points along a cubic bezier from `from` to `to` that leaves and arrives horizontally.
/// The control points sit exactly halfway, so curves that share a source or a target never
/// cross each other. The sample count follows the on-screen length so curves stay smooth
/// when zoomed in.
fn curve(from: Point<f32>, to: Point<f32>) -> Vec<Point<f32>> {
    let (dx, dy) = (to.x - from.x, to.y - from.y);
    let length = (dx * dx + dy * dy).sqrt();
    let samples = to_count(length / 9.0).clamp(8, 64);
    let reach = (dx * 0.5).max(20.0);
    let (c1, c2) = (point(from.x + reach, from.y), point(to.x - reach, to.y));
    (0..=samples)
        .map(|i| {
            let t = count(i) / count(samples);
            let u = 1.0 - t;
            let (w0, w1, w2, w3) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
            point(
                w0 * from.x + w1 * c1.x + w2 * c2.x + w3 * to.x,
                w0 * from.y + w1 * c1.y + w2 * c2.y + w3 * to.y,
            )
        })
        .collect()
}

fn count(n: usize) -> f32 {
    f32::from(u16::try_from(n).unwrap_or(u16::MAX))
}

/// Truncating float-to-count conversion; inputs are small and non-negative.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn to_count(value: f32) -> usize {
    value.max(0.0) as usize
}

fn disc(center: Point<f32>, radius: f32, color: Hsla, window: &mut Window) {
    let bounds = Bounds {
        origin: point(px(center.x - radius), px(center.y - radius)),
        size: size(px(2.0 * radius), px(2.0 * radius)),
    };
    window.paint_quad(quad(
        bounds,
        px(radius),
        color,
        px(0.0),
        color,
        BorderStyle::default(),
    ));
}

/// Stroke a polyline with round joins: one convex quad per segment plus a disc at each
/// interior joint. GPUI fills a path as a triangle fan from its first point, so concave
/// outlines cannot be used; small convex pieces are exact.
fn stroke(points: &[Point<f32>], width: f32, color: Hsla, window: &mut Window) {
    let half = width / 2.0;
    for pair in points.windows(2) {
        let (a, b) = (pair[0], pair[1]);
        let (dx, dy) = (b.x - a.x, b.y - a.y);
        let len = (dx * dx + dy * dy).sqrt();
        if len < 0.01 {
            continue;
        }
        let (nx, ny) = (-dy / len * half, dx / len * half);
        let mut path = Path::new(point(px(a.x + nx), px(a.y + ny)));
        path.line_to(point(px(b.x + nx), px(b.y + ny)));
        path.line_to(point(px(b.x - nx), px(b.y - ny)));
        path.line_to(point(px(a.x - nx), px(a.y - ny)));
        window.paint_path(path, color);
    }
    if width >= 2.5 {
        for joint in points.iter().skip(1).take(points.len().saturating_sub(2)) {
            disc(*joint, half, color, window);
        }
    }
}

/// A filled arrowhead whose axis follows the curve's final tangent.
fn arrowhead(tail: Point<f32>, tip: Point<f32>, length: f32, color: Hsla, window: &mut Window) {
    let (dx, dy) = (tip.x - tail.x, tip.y - tail.y);
    let len = (dx * dx + dy * dy).sqrt().max(0.001);
    let (ux, uy) = (dx / len, dy / len);
    let (nx, ny) = (-uy * length * 0.5, ux * length * 0.5);
    let base = point(tip.x - ux * length, tip.y - uy * length);
    let mut path = Path::new(point(px(tip.x), px(tip.y)));
    path.line_to(point(px(base.x + nx), px(base.y + ny)));
    path.line_to(point(px(base.x - nx), px(base.y - ny)));
    window.paint_path(path, color);
}

fn paint_edges(bounds: Bounds<Pixels>, camera: Camera, layout: &Layout, window: &mut Window) {
    let s = camera.scale;
    let screen = |x: f32, y: f32| {
        point(
            f32::from(bounds.origin.x) + x * s + camera.x,
            f32::from(bounds.origin.y) + y * s + camera.y,
        )
    };
    // Hierarchy first, grouped by parent, so dependency arrows read on top.
    let mut children: BTreeMap<usize, Vec<Point<f32>>> = BTreeMap::new();
    for edge in layout.edges.iter().filter(|edge| !edge.dependency) {
        let child = &layout.nodes[edge.to];
        children
            .entry(edge.from)
            .or_default()
            .push(screen(child.x - child.hw, child.y));
    }
    // Every edge leaves from the parent's rightmost point in the parent's colour and
    // weight; the halfway control points keep the fan from crossing itself.
    for (parent, targets) in &children {
        let node = &layout.nodes[*parent];
        let start = screen(node.x + node.hw, node.y);
        let width = edge_width(s);
        for end in targets {
            stroke(&curve(start, *end), width, black(), window);
        }
    }
    for edge in layout.edges.iter().filter(|edge| edge.dependency) {
        let (a, b) = (&layout.nodes[edge.from], &layout.nodes[edge.to]);
        let (width, color) = (edge_width(s), black());
        let start = screen(a.x + a.hw, a.y);
        let end = screen(b.x - b.hw, b.y);
        let end = point(end.x - 2.0 * s, end.y);
        let points = curve(start, end);
        let head = (width * 3.2).max(8.0);
        // Stop the stroke where the arrowhead begins so the tip stays crisp.
        let mut body = points.clone();
        if let Some(last) = body.last_mut() {
            *last = point(end.x - head * 0.8, end.y);
        }
        stroke(&body, width, color, window);
        arrowhead(points[points.len() - 2], end, head, color, window);
    }
}

fn text_run(window: &Window, len: usize, bold: bool) -> TextRun {
    let mut font = window.text_style().font();
    if bold {
        font.weight = FontWeight::BOLD;
    }
    TextRun {
        len,
        font,
        color: black(),
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// A block of wrapped text ready to paint, with its total height.
struct Block {
    lines: Vec<WrappedLine>,
    line_height: Pixels,
    height: Pixels,
}

fn shape_block(
    window: &Window,
    text: &str,
    font_size: f32,
    bold: bool,
    wrap_width: f32,
    clamp: usize,
) -> Option<Block> {
    if font_size < MIN_FONT_PX || text.trim().is_empty() {
        return None;
    }
    let text = SharedString::from(text.to_owned());
    let run = text_run(window, text.len(), bold);
    let lines = window
        .text_system()
        .shape_text(
            text,
            px(font_size),
            &[run],
            Some(px(wrap_width)),
            Some(clamp),
        )
        .ok()?
        .into_vec();
    let line_height = px(font_size * 1.25);
    let height = lines
        .iter()
        .map(|line| line.size(line_height).height)
        .fold(px(0.0), |acc, h| acc + h);
    Some(Block {
        lines,
        line_height,
        height,
    })
}

/// Paint a node's title, wrapped and centred inside its shape.
fn paint_node_text(
    center: Point<f32>,
    (hw, hh): (f32, f32),
    node: &Node,
    window: &mut Window,
    cx: &mut App,
) {
    // The largest comfortable text box inside each shape.
    let (box_w, box_h) = match node.kind {
        Kind::Project => (hw, hh),
        Kind::Goal => (hw * 1.8, hh * 1.7),
        Kind::Task => (hw * 1.32, hh * 1.32),
    };
    let font_size = match node.kind {
        Kind::Project => 0.2,
        Kind::Goal => 0.24,
        Kind::Task => 0.22,
    } * hh;
    let Some(block) = shape_block(window, &node.title, font_size, true, box_w, 4) else {
        return;
    };
    if f32::from(block.height) > box_h {
        return;
    }
    let x = px(center.x - box_w / 2.0);
    let mut y = px(center.y) - block.height / 2.0;
    let area = Bounds {
        origin: point(x, y),
        size: size(px(box_w), block.height),
    };
    for line in &block.lines {
        let _ = line.paint(
            point(x, y),
            block.line_height,
            TextAlign::Center,
            Some(area),
            window,
            cx,
        );
        y += line.size(block.line_height).height;
    }
}

/// Fill and outline one node shape. `fill` is `None` for the inner collapsed marker and
/// for legend miniatures; `outline` is black on the canvas and white in the legend.
fn paint_shape(
    kind: Kind,
    center: Point<f32>,
    (hw, hh): (f32, f32),
    fill: Option<Hsla>,
    width: f32,
    outline: Hsla,
    window: &mut Window,
) {
    match kind {
        Kind::Project => {
            let corners = [
                point(center.x - hw, center.y),
                point(center.x, center.y - hh),
                point(center.x + hw, center.y),
                point(center.x, center.y + hh),
            ];
            if let Some(fill) = fill {
                let mut path = Path::new(point(px(corners[0].x), px(corners[0].y)));
                for corner in &corners[1..] {
                    path.line_to(point(px(corner.x), px(corner.y)));
                }
                window.paint_path(path, fill);
            }
            let loop_ = [corners[0], corners[1], corners[2], corners[3], corners[0]];
            stroke(&loop_, width, outline, window);
            for corner in corners {
                disc(corner, width / 2.0, outline, window);
            }
        }
        Kind::Goal | Kind::Task => {
            let bounds = Bounds {
                origin: point(px(center.x - hw), px(center.y - hh)),
                size: size(px(2.0 * hw), px(2.0 * hh)),
            };
            let radius = if kind == Kind::Task { hw } else { hh * 0.18 };
            window.paint_quad(quad(
                bounds,
                px(radius),
                fill.unwrap_or_else(transparent_black),
                px(width),
                outline,
                BorderStyle::default(),
            ));
        }
    }
}

/// States that mean work is live on it right now, and so deserve a pulse.
fn pulses(state: State) -> bool {
    matches!(
        state,
        State::InWork | State::Validating | State::AwaitingMerge
    )
}

/// A soft, breathing halo behind a node: a few translucent copies of its shape, each a
/// little larger than the last, whose reach swells and shrinks with time.
fn paint_glow(
    node: &Node,
    center: Point<f32>,
    (hw, hh): (f32, f32),
    scale: f32,
    seconds: f32,
    window: &mut Window,
) {
    const PERIOD: f32 = 1.8;
    let pulse = (seconds * std::f32::consts::TAU / PERIOD).sin() * 0.5 + 0.5;
    // Reach follows zoom but never drops below a few screen pixels, so the pulse stays
    // visible in the overview.
    let reach = ((10.0 + 18.0 * pulse) * scale).max(4.0 + 8.0 * pulse);
    let mut color = fill_color(node.state);
    for step in (1..=4).rev() {
        let spread = reach * count(step) / 4.0;
        color.a = 0.32 * (1.0 - count(step - 1) / 4.0) * (0.5 + 0.5 * pulse);
        paint_shape(
            node.kind,
            center,
            (hw + spread, hh + spread),
            Some(color),
            0.0,
            transparent_black(),
            window,
        );
    }
}

fn paint_nodes(
    bounds: Bounds<Pixels>,
    camera: Camera,
    nodes: &[Node],
    seconds: f32,
    window: &mut Window,
    cx: &mut App,
) {
    let s = camera.scale;
    for node in nodes {
        let width = screen_width(node.kind, s);
        let (hw, hh) = (node.hw * s, node.hh * s);
        let center = point(
            f32::from(bounds.origin.x) + node.x * s + camera.x,
            f32::from(bounds.origin.y) + node.y * s + camera.y,
        );
        if pulses(node.state) {
            paint_glow(node, center, (hw, hh), s, seconds, window);
        }
        let fill = fill_color(node.state);
        paint_shape(
            node.kind,
            center,
            (hw, hh),
            Some(fill),
            width,
            darker(fill),
            window,
        );
        if node.collapsed {
            // A second, thinner outline marks hidden children.
            let inset = width * 2.5;
            paint_shape(
                node.kind,
                center,
                (hw - inset, hh - inset),
                None,
                (width * 0.5).max(1.0),
                darker(fill),
                window,
            );
        }
        paint_node_text(center, (hw, hh), node, window, cx);
    }
}

fn paint_graph(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    camera: Camera,
    seconds: f32,
    window: &mut Window,
    cx: &mut App,
) {
    window.with_content_mask(Some(ContentMask { bounds }), |window| {
        window.paint_quad(fill(bounds, white()));
        paint_edges(bounds, camera, layout, window);
        paint_nodes(bounds, camera, &layout.nodes, seconds, window, cx);
    });
    if layout.nodes.iter().any(|node| pulses(node.state)) {
        window.request_animation_frame();
    }
}

impl Render for Viewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .relative()
            .size_full()
            .bg(white())
            .text_color(black())
            .child(self.graph(cx))
            .children(self.panel(cx))
            .child(self.legend(cx))
    }
}

impl Viewer {
    fn graph(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let layout = Rc::clone(&self.layout);
        let shared = Rc::clone(&self.shared);
        let paint_layout = Rc::clone(&layout);
        let started = self.started;
        div()
            .id("graph")
            .size_full()
            .cursor(CursorStyle::OpenHand)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, _| {
                    this.press = Some(Press {
                        last: event.position,
                        dragged: false,
                    });
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some(press) = &mut this.press else {
                    return;
                };
                if event.pressed_button != Some(MouseButton::Left) {
                    this.press = None;
                    return;
                }
                let (dx, dy) = (
                    f32::from(event.position.x - press.last.x),
                    f32::from(event.position.y - press.last.y),
                );
                press.last = event.position;
                if dx.abs() + dy.abs() > 2.0 {
                    press.dragged = true;
                }
                this.update_shared(|shared| {
                    shared.camera.x += dx;
                    shared.camera.y += dy;
                });
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    if let Some(press) = this.press.take()
                        && !press.dragged
                    {
                        this.select_at(event.position);
                        cx.notify();
                    }
                }),
            )
            .on_mouse_up(
                MouseButton::Right,
                cx.listener(|this, event: &MouseUpEvent, _, cx| {
                    this.toggle_at(event.position);
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, _| this.press = None),
            )
            .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, _, cx| {
                let factor = match event.delta {
                    ScrollDelta::Lines(delta) => 1.12_f32.powf(delta.y),
                    ScrollDelta::Pixels(delta) => (f32::from(delta.y) / 160.0).exp(),
                };
                this.zoom_at(event.position, factor);
                cx.notify();
            }))
            .child(
                canvas(
                    move |bounds, _, _| {
                        let mut state = shared.get();
                        state.origin = bounds.origin;
                        if state.fit_pending && bounds.size.width > px(0.0) {
                            state.camera = Camera::fit(layout.bounds, bounds.size);
                            state.fit_pending = false;
                        }
                        shared.set(state);
                        (state.camera, started.elapsed().as_secs_f32())
                    },
                    move |bounds, (camera, seconds), window, cx| {
                        paint_graph(bounds, &paint_layout, camera, seconds, window, cx);
                    },
                )
                .size_full(),
            )
    }

    /// A tiny key button in the top-left that opens the legend; the legend itself when open.
    fn legend(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let toggle = cx.listener(|this, _: &ClickEvent, _, cx| {
            this.legend_open = !this.legend_open;
            cx.notify();
        });
        if !self.legend_open {
            return div()
                .id("key")
                .occlude()
                .absolute()
                .top_2()
                .left_2()
                .cursor_pointer()
                .p_1()
                .child(key_icon())
                .on_click(toggle);
        }
        div()
            .id("legend")
            .occlude()
            .absolute()
            .top_2()
            .left_2()
            .w(px(280.0))
            .p_3()
            .rounded_md()
            .bg(black())
            .text_color(white())
            .border_1()
            .border_color(rgb(0x0033_3333))
            .flex()
            .flex_col()
            .gap_2()
            .child(
                // The close button sits where the key icon was, so open and close are in
                // the same place.
                div().flex().justify_start().child(
                    div()
                        .id("close-key")
                        .cursor_pointer()
                        .px_2()
                        .rounded_md()
                        .hover(|style| style.bg(rgb(0x0033_3333)))
                        .child("×")
                        .on_click(toggle),
                ),
            )
            .child(shape_row(Kind::Project, "Project"))
            .child(shape_row(Kind::Goal, "Goal"))
            .child(shape_row(Kind::Task, "Task"))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap_2()
                    .mt_1()
                    .children(COLOUR_KEY.iter().map(|&(label, state)| badge(label, state))),
            )
    }

    /// The details panel for the selected node, overlaid on the right.
    fn panel(&self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let id = self.selected.as_deref()?;
        let body = details_for(&self.snapshot, id)?;
        let close = div()
            .id("close")
            .cursor_pointer()
            .px_2()
            .rounded_md()
            .text_lg()
            .hover(|style| style.bg(rgb(0x0033_3333)))
            .child("×")
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.selected = None;
                cx.notify();
            }));
        Some(
            div()
                .id("panel")
                .occlude()
                .absolute()
                .top_0()
                .right_0()
                .h_full()
                .w(px(380.0))
                .overflow_y_scroll()
                .p_4()
                .bg(black())
                .text_color(white())
                .border_l_1()
                .border_color(rgb(0x0033_3333))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .mb_3()
                        .child(
                            div()
                                .text_xs()
                                .text_color(rgb(LABEL_GREY))
                                .child(body.heading),
                        )
                        .child(close),
                )
                .child(body.content),
        )
    }
}

const LABEL_GREY: u32 = 0x009c_a3af;

/// Legend badges: label and state, in lifecycle order.
const COLOUR_KEY: [(&str, State); 7] = [
    ("ready", State::Open),
    ("blocked", State::Blocked),
    ("in progress", State::InWork),
    ("testing", State::Validating),
    ("ready for merge", State::AwaitingMerge),
    ("done", State::Finished),
    ("cancelled", State::Abandoned),
];

/// Icons embedded in the binary and served to GPUI's `svg` element by path.
struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "icons/key.svg" => Some(Cow::Borrowed(include_bytes!("../assets/icons/key.svg"))),
            _ => None,
        })
    }

    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(vec!["icons/key.svg".into()])
    }
}

/// The key icon, rendered from an embedded outline SVG and tinted black: a tilted key with
/// the bow at the upper right. The interior is unpainted so it reads as white on the canvas.
fn key_icon() -> impl IntoElement {
    svg()
        .path("icons/key.svg")
        .size(px(34.0))
        .text_color(black())
}

/// A legend row: a miniature of the node shape for `kind` beside its name.
fn shape_row(kind: Kind, label: &'static str) -> Div {
    let icon = canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let (hw, hh) = match kind {
                Kind::Project => (15.0, 9.0),
                Kind::Goal => (13.0, 8.0),
                Kind::Task => (8.0, 8.0),
            };
            let center = point(
                f32::from(bounds.origin.x) + f32::from(bounds.size.width) / 2.0,
                f32::from(bounds.origin.y) + f32::from(bounds.size.height) / 2.0,
            );
            paint_shape(kind, center, (hw, hh), None, 1.5, white(), window);
        },
    )
    .w(px(34.0))
    .h(px(22.0));
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(icon)
        .child(label)
}

/// A labelled section of the details panel holding any content.
fn labelled(label: &'static str, content: impl IntoElement) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(div().text_xs().text_color(rgb(LABEL_GREY)).child(label))
        .child(content)
}

/// A labelled section holding plain text; empty text shows as a dash.
fn section(label: &'static str, body: impl Into<SharedString>) -> Div {
    let body: SharedString = body.into();
    labelled(
        label,
        div().text_sm().child(if body.is_empty() {
            SharedString::from("—")
        } else {
            body
        }),
    )
}

/// The same hue at reduced lightness, used to outline badges and nodes. White has no hue to
/// keep, so it outlines in black.
fn darker(color: Hsla) -> Hsla {
    if color.s < 0.01 && color.l > 0.99 {
        return black();
    }
    Hsla {
        l: color.l * 0.62,
        ..color
    }
}

/// A status pill in the same colour as the circle fill for that state, outlined in a darker
/// shade of that colour, with dark text.
fn badge(label: impl Into<SharedString>, state: State) -> Div {
    div()
        .px_2()
        .rounded_full()
        .text_xs()
        .bg(fill_color(state))
        .border_1()
        .border_color(darker(fill_color(state)))
        .text_color(black())
        .whitespace_nowrap()
        .child(label.into())
}

/// A row of badge plus text, used for lists of related entities.
fn row(pill: Div, text: impl Into<SharedString>) -> Div {
    div()
        .flex()
        .items_center()
        .gap_2()
        .text_sm()
        .child(pill)
        .child(text.into())
}

/// A vertical list of rows; an empty list shows as a dash.
fn list(rows: Vec<Div>) -> Div {
    if rows.is_empty() {
        return div().text_sm().child("—");
    }
    div().flex().flex_col().gap_1().children(rows)
}

fn title(text: &str) -> Div {
    div()
        .text_lg()
        .font_weight(FontWeight::BOLD)
        .child(text.to_owned())
}

/// Human wording for a status, with the ready/blocked split for todo tasks.
fn task_state_label(task: &TaskDetail) -> String {
    match task.task.status {
        TaskStatus::Todo if task.ready => "ready".into(),
        TaskStatus::Todo => "blocked".into(),
        status => status.as_str().replace('_', " "),
    }
}

/// "Project name › Goal title" for a task, using titles rather than slugs.
fn location(snapshot: &Snapshot, task: &TaskDetail) -> String {
    let goal = snapshot.goals.iter().find(|g| g.id == task.task.goal_id);
    let project = goal.and_then(|g| snapshot.projects.iter().find(|p| p.id == g.project_id));
    format!(
        "{} › {}",
        project.map_or(task.project.as_str(), |p| p.name.as_str()),
        goal.map_or(task.goal.as_str(), |g| g.title.as_str())
    )
}

fn task_badge(task: &TaskDetail) -> Div {
    badge(task_state_label(task), State::of_task(task))
}

fn goal_badge(goal: &Goal) -> Div {
    badge(goal.status.as_str().to_owned(), goal.status.into())
}

struct Details {
    heading: String,
    content: Div,
}

fn details_for(snapshot: &Snapshot, id: &str) -> Option<Details> {
    if let Some(task) = snapshot.tasks.iter().find(|t| t.task.id == id) {
        return Some(task_details(snapshot, task));
    }
    if let Some(goal) = snapshot.goals.iter().find(|g| g.id == id) {
        return Some(goal_details(snapshot, goal));
    }
    snapshot
        .projects
        .iter()
        .find(|p| p.id == id)
        .map(|project| project_details(snapshot, project))
}

fn task_details(snapshot: &Snapshot, detail: &TaskDetail) -> Details {
    let related = |ids: &[String]| {
        list(
            ids.iter()
                .filter_map(|id| snapshot.tasks.iter().find(|t| t.task.id == *id))
                .map(|t| row(task_badge(t), t.task.title.clone()))
                .collect(),
        )
    };
    let links = detail
        .links
        .iter()
        .map(|link| format!("{}: {}", link.kind, link.reference))
        .collect::<Vec<_>>()
        .join("\n");
    let content = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(title(&detail.task.title))
        .child(labelled("Status", div().flex().child(task_badge(detail))))
        .child(section("Where", location(snapshot, detail)))
        .child(section("Body", detail.task.body.clone()))
        .child(section("Test plan", detail.task.test_plan.clone()))
        .child(section(
            "Pull request",
            detail.task.pr.clone().unwrap_or_default(),
        ))
        .child(labelled("Depends on", related(&detail.depends_on)))
        .child(labelled("Blocked by", related(&detail.blocked_by)))
        .child(labelled("Dependents", related(&detail.dependents)))
        .child(section("Links", links))
        .child(section("Created", detail.task.created_at.to_string()))
        .child(section("Updated", detail.task.updated_at.to_string()))
        .child(section(
            "Completed",
            detail
                .task
                .completed_at
                .map(|t| t.to_string())
                .unwrap_or_default(),
        ));
    Details {
        heading: "Task".to_owned(),
        content,
    }
}

fn goal_details(snapshot: &Snapshot, goal: &Goal) -> Details {
    let tasks = list(
        snapshot
            .tasks
            .iter()
            .filter(|t| t.task.goal_id == goal.id)
            .map(|t| row(task_badge(t), t.task.title.clone()))
            .collect(),
    );
    let content = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(title(&goal.title))
        .child(labelled("Status", div().flex().child(goal_badge(goal))))
        .child(section("Description", goal.description.clone()))
        .child(section("Spec", goal.spec.clone().unwrap_or_default()))
        .child(labelled("Tasks", tasks))
        .child(section("Created", goal.created_at.to_string()))
        .child(section("Updated", goal.updated_at.to_string()))
        .child(section(
            "Completed",
            goal.completed_at.map(|t| t.to_string()).unwrap_or_default(),
        ));
    Details {
        heading: "Goal".to_owned(),
        content,
    }
}

fn project_details(snapshot: &Snapshot, project: &Project) -> Details {
    let goals = list(
        snapshot
            .goals
            .iter()
            .filter(|g| g.project_id == project.id)
            .map(|g| row(goal_badge(g), g.title.clone()))
            .collect(),
    );
    let content = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(title(&project.name))
        .child(section(
            "Repository path",
            project.repo_path.clone().unwrap_or_default(),
        ))
        .child(section(
            "Repository URL",
            project.repo_url.clone().unwrap_or_default(),
        ))
        .child(labelled("Goals", goals))
        .child(section("Created", project.created_at.to_string()));
    Details {
        heading: "Project".to_owned(),
        content,
    }
}

/// Open the database at `db` and run the viewer until its window closes.
///
/// # Errors
/// Returns an error if the database cannot be opened or read.
///
/// # Panics
/// Panics if the platform refuses to open a window, which GPUI reports only after the
/// application loop has started and so cannot be returned as an error.
pub fn run(db: &std::path::Path) -> anyhow::Result<()> {
    let mut store = Store::open(db)?;
    let snapshot = Snapshot::load(&mut store)?;
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            let bounds = Bounds::centered(None, size(px(1280.), px(820.)), cx);
            cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(bounds)),
                    ..Default::default()
                },
                |_, cx| cx.new(|_| Viewer::new(snapshot)),
            )
            .expect("open Tasky window");
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            cx.activate(true);
        });
    Ok(())
}
