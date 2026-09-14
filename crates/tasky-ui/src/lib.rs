//! Read-only GPUI viewer: a pannable, zoomable graph of every project, goal, and task,
//! launched from the CLI with `tasky ui`.

mod layout;

use gpui::{
    App, Application, AssetSource, BorderStyle, Bounds, ClickEvent, ClipboardItem, ContentMask,
    Context, CursorStyle, Div, FocusHandle, FontWeight, HighlightStyle, Hsla, KeyDownEvent,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Path, Pixels, Point, ScrollDelta,
    ScrollWheelEvent, SharedString, Size, Stateful, StyledText, TextAlign, TextLayout, TextRun,
    Window, WindowBounds, WindowOptions, WrappedLine, black, canvas, div, fill, hsla, point,
    prelude::*, px, quad, rgb, size, svg, transparent_black, white,
};
use layout::{Kind, Layout, Node, Rect, State};
use std::{
    borrow::Cow,
    cell::Cell,
    collections::{BTreeMap, HashSet},
    ops::Range,
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
    /// Keyboard focus, so copy shortcuts reach the viewer.
    focus: FocusHandle,
    /// Highlighted text in the details panel.
    selection: Option<Selection>,
    /// The left button is held after starting a selection, so moves extend it.
    selecting: bool,
    /// Every selectable text field in the panel as of the latest frame, by field id.
    fields: Vec<Field>,
}

impl Viewer {
    fn new(snapshot: Snapshot, cx: &mut Context<Self>) -> Self {
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
            focus: cx.focus_handle(),
            selection: None,
            selecting: false,
            fields: Vec::new(),
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
        self.selection = None;
    }

    /// The panel field under a window position, if any.
    fn field_at(&self, position: Point<Pixels>) -> Option<usize> {
        self.fields
            .iter()
            .position(|field| field.layout.bounds().contains(&position))
    }

    /// The byte offset in a field nearest to a window position.
    fn offset_in(&self, field: usize, position: Point<Pixels>) -> usize {
        self.fields[field]
            .layout
            .index_for_position(position)
            .unwrap_or_else(|nearest| nearest)
    }

    /// Left button pressed over the panel: start a selection on the field under the mouse,
    /// or clear the selection when there is none. A double click takes the word, a triple
    /// click the whole field. Returns whether a field was hit.
    fn begin_selection(&mut self, event: &MouseDownEvent) -> bool {
        let Some(field) = self.field_at(event.position) else {
            self.selection = None;
            return false;
        };
        let offset = self.offset_in(field, event.position);
        let text = &self.fields[field].text;
        let (anchor, head) = match event.click_count {
            1 => (offset, offset),
            2 => word_at(text, offset),
            _ => (0, text.len()),
        };
        self.selection = Some(Selection {
            field,
            anchor,
            head,
        });
        self.selecting = event.click_count == 1;
        true
    }

    /// Mouse moved while a selection is being dragged out: follow it. Registered on both
    /// the root and the panel, since the panel occludes hover on the root beneath it.
    fn drag_selection(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        if event.pressed_button != Some(MouseButton::Left) {
            self.selecting = false;
            return;
        }
        self.extend_selection(event.position);
        cx.notify();
    }

    /// Extend the selection being dragged out to the mouse position.
    fn extend_selection(&mut self, position: Point<Pixels>) {
        if let Some(selection) = &mut self.selection {
            selection.head = self.fields[selection.field]
                .layout
                .index_for_position(position)
                .unwrap_or_else(|nearest| nearest);
        }
    }

    /// Release after a drag: on platforms with a primary selection, the text goes there.
    fn finish_selection(&mut self, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        if let Some(text) = self.selected_text() {
            cx.write_to_primary(ClipboardItem::new_string(text));
        }
    }

    fn selected_text(&self) -> Option<String> {
        let selection = self.selection?;
        let range = selection.range();
        if range.is_empty() {
            return None;
        }
        Some(self.fields[selection.field].text[range].to_owned())
    }

    /// Ctrl-C or Cmd-C copies the selection; Escape clears it.
    fn key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let copy = keystroke.modifiers.control || keystroke.modifiers.platform;
        match keystroke.key.as_str() {
            "c" if copy => {
                if let Some(text) = self.selected_text() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
            "escape" => {
                self.selection = None;
                cx.notify();
            }
            _ => {}
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
) -> Option<Block> {
    if font_size < MIN_FONT_PX || text.trim().is_empty() {
        return None;
    }
    let text = SharedString::from(text.to_owned());
    let run = text_run(window, text.len(), bold);
    let lines = window
        .text_system()
        .shape_text(text, px(font_size), &[run], Some(px(wrap_width)), None)
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

/// Shape `text` wrapped to `box_w`, but only if the whole block also fits in `box_h`.
fn shape_fitting(
    window: &Window,
    text: &str,
    font_size: f32,
    (box_w, box_h): (f32, f32),
) -> Option<Block> {
    shape_block(window, text, font_size, true, box_w)
        .filter(|block| f32::from(block.height) <= box_h)
}

/// Shape a title to fit a text box, truncating it with an ellipsis when the full text
/// would spill past the box. Returns `None` when not even the ellipsis fits.
fn shape_title(window: &Window, text: &str, font_size: f32, text_box: (f32, f32)) -> Option<Block> {
    if let Some(block) = shape_fitting(window, text, font_size, text_box) {
        return Some(block);
    }
    // Byte offset of every character boundary, so prefixes never split a character.
    let ends: Vec<usize> = text
        .char_indices()
        .map(|(i, _)| i)
        .chain([text.len()])
        .collect();
    let candidate = |chars: usize| format!("{}\u{2026}", text[..ends[chars]].trim_end());
    // Binary search for the longest prefix that still fits once the ellipsis is appended:
    // keeping 0 characters is the lone ellipsis, keeping `n` is the first `n` characters.
    let (mut lo, mut hi) = (0, ends.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        if shape_fitting(window, &candidate(mid), font_size, text_box).is_some() {
            lo = mid;
        } else {
            hi = mid - 1;
        }
    }
    shape_fitting(window, &candidate(lo), font_size, text_box)
}

/// Paint a node's title, wrapped and centred inside its shape, truncated with an
/// ellipsis when it would overflow.
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
    let Some(block) = shape_title(window, &node.title, font_size, (box_w, box_h)) else {
        return;
    };
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus.is_focused(window) {
            window.focus(&self.focus);
        }
        let panel = self.panel(cx);
        div()
            .relative()
            .size_full()
            .bg(white())
            .text_color(black())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                this.key_down(event, cx);
            }))
            .on_mouse_move(cx.listener(Self::drag_selection))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_selection(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.finish_selection(cx)),
            )
            .child(self.graph(cx))
            .children(panel)
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
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.press = Some(Press {
                        last: event.position,
                        dragged: false,
                    });
                    if this.selection.take().is_some() {
                        cx.notify();
                    }
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
    fn panel(&mut self, cx: &mut Context<Self>) -> Option<Stateful<Div>> {
        let mut panel = Panel {
            snapshot: &self.snapshot,
            selection: self.selection,
            fields: Vec::new(),
        };
        let body = self
            .selected
            .as_deref()
            .and_then(|id| details_for(&mut panel, id));
        self.fields = panel.fields;
        let body = body?;
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
                this.selection = None;
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
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, event: &MouseDownEvent, _, cx| {
                        if this.begin_selection(event) {
                            cx.stop_propagation();
                        }
                        cx.notify();
                    }),
                )
                .on_mouse_move(cx.listener(Self::drag_selection))
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

/// Highlight behind selected panel text.
fn selection_color() -> Hsla {
    hsla(0.58, 0.9, 0.55, 0.6)
}

/// The byte range of the word around `offset`, or of the gap when `offset` is between words.
fn word_at(text: &str, offset: usize) -> (usize, usize) {
    let is_word = |c: char| c.is_alphanumeric() || c == '_';
    let start = text[..offset]
        .char_indices()
        .rev()
        .take_while(|&(_, c)| is_word(c))
        .last()
        .map_or(offset, |(i, _)| i);
    let end = text[offset..]
        .char_indices()
        .find(|&(_, c)| !is_word(c))
        .map_or(text.len(), |(i, _)| offset + i);
    (start, end)
}

/// A run of selected text in the details panel: the field it lives in and the byte
/// offsets where the selection started and where it currently ends.
#[derive(Debug, Clone, Copy)]
struct Selection {
    field: usize,
    anchor: usize,
    head: usize,
}

impl Selection {
    fn range(self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

/// A selectable text field in the details panel and its layout from the frame that built
/// it, which maps mouse positions to byte offsets.
struct Field {
    text: SharedString,
    layout: TextLayout,
}

/// Builds the details panel for one frame, handing out a field id to every piece of
/// selectable text and painting the current selection into it.
struct Panel<'a> {
    snapshot: &'a Snapshot,
    selection: Option<Selection>,
    fields: Vec<Field>,
}

impl Panel<'_> {
    /// Plain text the user can select and copy.
    fn text(&mut self, text: impl Into<SharedString>) -> Div {
        let text: SharedString = text.into();
        let id = self.fields.len();
        let highlight = self
            .selection
            .filter(|selection| selection.field == id)
            .map(Selection::range)
            .filter(|range| !range.is_empty())
            .map(|range| {
                (
                    range,
                    HighlightStyle {
                        background_color: Some(selection_color()),
                        ..HighlightStyle::default()
                    },
                )
            });
        let styled = StyledText::new(text.clone()).with_highlights(highlight);
        self.fields.push(Field {
            text,
            layout: styled.layout().clone(),
        });
        div().cursor_text().child(styled)
    }

    fn title(&mut self, text: &str) -> Div {
        self.text(text.to_owned())
            .text_lg()
            .font_weight(FontWeight::BOLD)
    }

    /// A labelled section holding plain text; empty text shows as a dash.
    fn section(&mut self, label: &'static str, body: impl Into<SharedString>) -> Div {
        let body: SharedString = body.into();
        let body = if body.is_empty() {
            SharedString::from("—")
        } else {
            body
        };
        labelled(label, self.text(body).text_sm())
    }

    /// A row of badge plus text, used for lists of related entities.
    fn row(&mut self, pill: Div, text: impl Into<SharedString>) -> Div {
        div()
            .flex()
            .items_center()
            .gap_2()
            .text_sm()
            .child(pill)
            .child(self.text(text))
    }

    /// Rows for the tasks with the given IDs, in that order.
    fn tasks(&mut self, ids: &[String]) -> Div {
        let snapshot = self.snapshot;
        list(
            ids.iter()
                .filter_map(|id| snapshot.tasks.iter().find(|t| t.task.id == *id))
                .map(|t| self.row(task_badge(t), t.task.title.clone()))
                .collect(),
        )
    }
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

/// A vertical list of rows; an empty list shows as a dash.
fn list(rows: Vec<Div>) -> Div {
    if rows.is_empty() {
        return div().text_sm().child("—");
    }
    div().flex().flex_col().gap_1().children(rows)
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

fn details_for(panel: &mut Panel<'_>, id: &str) -> Option<Details> {
    let snapshot = panel.snapshot;
    if let Some(task) = snapshot.tasks.iter().find(|t| t.task.id == id) {
        return Some(task_details(panel, task));
    }
    if let Some(goal) = snapshot.goals.iter().find(|g| g.id == id) {
        return Some(goal_details(panel, goal));
    }
    snapshot
        .projects
        .iter()
        .find(|p| p.id == id)
        .map(|project| project_details(panel, project))
}

fn task_details(panel: &mut Panel<'_>, detail: &TaskDetail) -> Details {
    let snapshot = panel.snapshot;
    let depends_on = panel.tasks(&detail.depends_on);
    let blocked_by = panel.tasks(&detail.blocked_by);
    let dependents = panel.tasks(&detail.dependents);
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
        .child(panel.title(&detail.task.title))
        .child(labelled("Status", div().flex().child(task_badge(detail))))
        .child(panel.section("Where", location(snapshot, detail)))
        .child(panel.section("Body", detail.task.body.clone()))
        .child(panel.section("Test plan", detail.task.test_plan.clone()))
        .child(panel.section("Pull request", detail.task.pr.clone().unwrap_or_default()))
        .child(labelled("Depends on", depends_on))
        .child(labelled("Blocked by", blocked_by))
        .child(labelled("Dependents", dependents))
        .child(panel.section("Links", links))
        .child(panel.section("Created", detail.task.created_at.to_string()))
        .child(panel.section("Updated", detail.task.updated_at.to_string()))
        .child(
            panel.section(
                "Completed",
                detail
                    .task
                    .completed_at
                    .map(|t| t.to_string())
                    .unwrap_or_default(),
            ),
        );
    Details {
        heading: "Task".to_owned(),
        content,
    }
}

fn goal_details(panel: &mut Panel<'_>, goal: &Goal) -> Details {
    let snapshot = panel.snapshot;
    let tasks = list(
        snapshot
            .tasks
            .iter()
            .filter(|t| t.task.goal_id == goal.id)
            .map(|t| panel.row(task_badge(t), t.task.title.clone()))
            .collect(),
    );
    let parent = goal
        .parent_id
        .as_deref()
        .and_then(|id| snapshot.goals.iter().find(|g| g.id == id))
        .map(|g| g.title.clone())
        .unwrap_or_default();
    let subgoals = list(
        snapshot
            .goals
            .iter()
            .filter(|g| g.parent_id.as_deref() == Some(goal.id.as_str()))
            .map(|g| panel.row(goal_badge(g), g.title.clone()))
            .collect(),
    );
    let content = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(panel.title(&goal.title))
        .child(labelled("Status", div().flex().child(goal_badge(goal))))
        .child(panel.section("Parent goal", parent))
        .child(panel.section("Description", goal.description.clone()))
        .child(panel.section("Spec", goal.spec.clone().unwrap_or_default()))
        .child(labelled("Sub-goals", subgoals))
        .child(labelled("Tasks", tasks))
        .child(panel.section("Created", goal.created_at.to_string()))
        .child(panel.section("Updated", goal.updated_at.to_string()))
        .child(panel.section(
            "Completed",
            goal.completed_at.map(|t| t.to_string()).unwrap_or_default(),
        ));
    Details {
        heading: "Goal".to_owned(),
        content,
    }
}

fn project_details(panel: &mut Panel<'_>, project: &Project) -> Details {
    let snapshot = panel.snapshot;
    let goals = list(
        snapshot
            .goals
            .iter()
            .filter(|g| g.project_id == project.id && g.parent_id.is_none())
            .map(|g| panel.row(goal_badge(g), g.title.clone()))
            .collect(),
    );
    let parent = project
        .parent_id
        .as_deref()
        .and_then(|id| snapshot.projects.iter().find(|p| p.id == id))
        .map(|p| p.name.clone())
        .unwrap_or_default();
    let subprojects = snapshot
        .projects
        .iter()
        .filter(|p| p.parent_id.as_deref() == Some(project.id.as_str()))
        .map(|p| p.name.clone())
        .collect::<Vec<_>>()
        .join("\n");
    let content = div()
        .flex()
        .flex_col()
        .gap_3()
        .child(panel.title(&project.name))
        .child(panel.section("Parent project", parent))
        .child(panel.section("Sub-projects", subprojects))
        .child(panel.section(
            "Repository path",
            project.repo_path.clone().unwrap_or_default(),
        ))
        .child(panel.section(
            "Repository URL",
            project.repo_url.clone().unwrap_or_default(),
        ))
        .child(labelled("Goals", goals))
        .child(panel.section("Created", project.created_at.to_string()));
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
                |_, cx| cx.new(|cx| Viewer::new(snapshot, cx)),
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
