use gpui::{
    App, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions, point, prelude::*, px,
    size,
};

use crate::assets::Assets;
use crate::fonts;
use crate::services::window_state::{self, Rect, WindowMode};
use crate::views::terminal_manager::TerminalManager;

pub fn run() {
    let cwd = std::env::current_dir().unwrap_or_else(|_| ".".into());
    let (working_directory, requested_project) =
        window_state::resolve_launch_directory(std::env::args_os().skip(1), &cwd);
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            crate::services::accessibility::refresh_motion_preference();
            let font_catalog = fonts::initialize(cx);
            crate::views::file_editor::FileEditor::initialize(cx);
            TerminalManager::bind_keys(cx);
            let storage_path = font_catalog
                .settings_path
                .with_file_name("terminal-workspace.json");
            let window_state_path = storage_path.with_file_name("window-state.json");
            let saved_window = window_state::load(&window_state_path).ok().flatten();
            let displays: Vec<Rect> = cx
                .displays()
                .into_iter()
                .map(|display| {
                    let bounds = display.bounds();
                    Rect::new(
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    )
                })
                .collect();
            let primary = cx
                .primary_display()
                .map(|display| {
                    let bounds = display.bounds();
                    Rect::new(
                        f32::from(bounds.origin.x),
                        f32::from(bounds.origin.y),
                        f32::from(bounds.size.width),
                        f32::from(bounds.size.height),
                    )
                })
                .unwrap_or_else(|| {
                    Rect::new(
                        0.0,
                        0.0,
                        window_state::DEFAULT_WINDOW_WIDTH,
                        window_state::DEFAULT_WINDOW_HEIGHT,
                    )
                });
            let (rect, mode) =
                window_state::resolve_window_bounds(saved_window, &displays, primary);
            let bounds = Bounds::new(
                point(px(rect.x), px(rect.y)),
                size(px(rect.width), px(rect.height)),
            );
            let window_bounds = match mode {
                WindowMode::Windowed => WindowBounds::Windowed(bounds),
                WindowMode::Maximized => WindowBounds::Maximized(bounds),
            };
            cx.on_window_closed(|cx| {
                if cx.windows().is_empty() {
                    cx.quit();
                }
            })
            .detach();
            let opened = cx.open_window(
                WindowOptions {
                    window_bounds: Some(window_bounds),
                    window_min_size: Some(size(
                        px(window_state::MIN_WINDOW_WIDTH),
                        px(window_state::MIN_WINDOW_HEIGHT),
                    )),
                    titlebar: Some(TitlebarOptions {
                        title: Some("Zeod — Terminals".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    app_id: Some("zeod".into()),
                    ..Default::default()
                },
                move |window, cx| {
                    let root = cx.new(|cx| {
                        TerminalManager::new(
                            working_directory.clone(),
                            requested_project.clone(),
                            storage_path.clone(),
                            window,
                            cx,
                        )
                    });
                    let weak = root.downgrade();
                    window.on_window_should_close(cx, move |window, cx| {
                        weak.update(cx, |root, cx| root.request_close(window, cx))
                            .unwrap_or(true)
                    });
                    cx.new(|cx| gpui_component::Root::new(root, window, cx))
                },
            );
            if let Err(error) = opened {
                eprintln!("The terminal window could not be opened: {error}");
                cx.quit();
                return;
            }
            cx.activate(true);
        });
}
