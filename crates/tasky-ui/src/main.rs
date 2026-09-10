use clap::Parser;
use gpui::{
    App, Application, Bounds, Context, Window, WindowBounds, WindowOptions, div, prelude::*, px,
    rgb, size,
};
use std::path::PathBuf;
use tasky_core::{Graph, Status};
use tasky_store::Store;

#[derive(Parser)]
#[command(version, about = "Read-only Tasky graph viewer")]
struct Args {
    #[arg(long, default_value = ".tasky")]
    store: PathBuf,
}

struct Viewer {
    store: Store,
    graph: Graph,
    error: Option<String>,
}

impl Render for Viewer {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let rows = self
            .graph
            .tasks()
            .map(|task| {
                let (state, color) = match &task.status {
                    Status::Pending if self.graph.is_ready(task) => ("ready".into(), 0x4ade80),
                    Status::Pending => ("blocked".into(), 0xfbbf24),
                    Status::Running { agent } => (format!("running / {agent}"), 0x60a5fa),
                    Status::Done => ("done".into(), 0xa1a1aa),
                    Status::Failed { reason } => (format!("failed / {reason}"), 0xf87171),
                };
                let dependencies = task
                    .dependencies
                    .iter()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", ");
                div()
                    .p_3()
                    .mb_2()
                    .rounded_md()
                    .bg(rgb(0x202938))
                    .child(
                        div()
                            .text_color(rgb(color))
                            .child(format!("{} · {}", task.id, state)),
                    )
                    .child(task.title.clone())
                    .child(div().text_sm().text_color(rgb(0x9ca3af)).child(format!(
                        "Requires: {}",
                        if dependencies.is_empty() {
                            "none"
                        } else {
                            &dependencies
                        }
                    )))
            })
            .collect::<Vec<_>>();

        div()
            .size_full()
            .flex()
            .flex_col()
            .p_6()
            .gap_3()
            .bg(rgb(0x111827))
            .text_color(rgb(0xf3f4f6))
            .child(div().text_xl().child("Tasky / task graph"))
            .child(format!(
                "{} tasks · {} ready",
                self.graph.tasks().count(),
                self.graph.ready().count()
            ))
            .child(
                div()
                    .id("refresh")
                    .cursor_pointer()
                    .p_2()
                    .rounded_md()
                    .bg(rgb(0x374151))
                    .child("Refresh snapshot")
                    .on_click(cx.listener(|this, _, _, cx| {
                        match this.store.load() {
                            Ok(graph) => {
                                this.graph = graph;
                                this.error = None;
                            }
                            Err(error) => {
                                this.error = Some(format!(
                                    "Refresh failed (showing previous snapshot): {error:#}"
                                ))
                            }
                        }
                        cx.notify();
                    })),
            )
            .child(self.error.clone().unwrap_or_default())
            .child(
                div()
                    .id("tasks")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(if rows.is_empty() {
                        "No tasks yet. Add tasks using the CLI."
                    } else {
                        ""
                    })
                    .children(rows),
            )
    }
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    let store = Store::new(args.store);
    let graph = store.load()?;
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(900.), px(680.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| {
                cx.new(|_| Viewer {
                    store,
                    graph,
                    error: None,
                })
            },
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
