use tauri::{AppHandle, Manager, Position, Size};

fn monitor_contains_physical_point(
    origin_x: f64,
    origin_y: f64,
    width: f64,
    height: f64,
    point_x: f64,
    point_y: f64,
) -> bool {
    point_x >= origin_x
        && point_x < origin_x + width
        && point_y >= origin_y
        && point_y < origin_y + height
}

mod platform {
    use super::*;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::Once;
    use std::time::{SystemTime, UNIX_EPOCH};

    static INIT_DONE: Once = Once::new();
    static PANEL_READY: AtomicBool = AtomicBool::new(false);
    static LAST_SHOW_MS: AtomicU64 = AtomicU64::new(0);
    const SHOW_GRACE_MS: u64 = 1000;

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn init(app_handle: &AppHandle) -> tauri::Result<()> {
        INIT_DONE.call_once(|| {
            let Some(window) = app_handle.get_webview_window("main") else {
                log::error!("[linux panel] init: main window not found");
                return;
            };

            if let Err(e) = window.set_always_on_top(true) {
                log::warn!("[linux panel] set_always_on_top: {}", e);
            }
            if let Err(e) = window.set_skip_taskbar(true) {
                log::warn!("[linux panel] set_skip_taskbar: {}", e);
            }

            let handle = app_handle.clone();
            let win = window.clone();
            win.on_window_event(move |event| {
                match event {
                    tauri::WindowEvent::Focused(focused) => {
                        log::debug!("[linux panel] Focused({})", focused);
                        if !focused {
                            let elapsed =
                                now_ms().saturating_sub(LAST_SHOW_MS.load(Ordering::Relaxed));
                            if elapsed < SHOW_GRACE_MS {
                                log::debug!(
                                    "[linux panel] focus loss ignored ({}ms < {}ms grace)",
                                    elapsed,
                                    SHOW_GRACE_MS
                                );
                                return;
                            }
                            if let Some(w) = handle.get_webview_window("main") {
                                log::debug!("[linux panel] hiding on focus loss");
                                let _ = w.hide();
                            }
                        }
                    }
                    _ => {}
                }
            });

            PANEL_READY.store(true, Ordering::Relaxed);
            log::info!("[linux panel] initialized");
        });
        if PANEL_READY.load(Ordering::Relaxed) {
            Ok(())
        } else {
            Err(tauri::Error::WindowNotFound)
        }
    }

    pub fn show(app_handle: &AppHandle) {
        let Some(window) = app_handle.get_webview_window("main") else {
            log::error!("[linux panel] show: main window not found");
            return;
        };
        if let Err(e) = init(app_handle) {
            log::error!("[linux panel] show: init failed: {}", e);
            return;
        }
        LAST_SHOW_MS.store(now_ms(), Ordering::Relaxed);
        match window.show() {
            Ok(()) => log::info!("[linux panel] window.show() ok"),
            Err(e) => log::error!("[linux panel] window.show() failed: {}", e),
        }
        match window.set_focus() {
            Ok(()) => log::info!("[linux panel] window.set_focus() ok"),
            Err(e) => log::warn!("[linux panel] window.set_focus() failed: {}", e),
        }
    }

    pub fn hide(app_handle: &AppHandle) {
        if let Some(window) = app_handle.get_webview_window("main") {
            log::debug!("[linux panel] hiding");
            let _ = window.hide();
        }
    }

    pub fn is_visible(app_handle: &AppHandle) -> bool {
        let vis = app_handle
            .get_webview_window("main")
            .and_then(|w| w.is_visible().ok())
            .unwrap_or(false);
        log::debug!("[linux panel] is_visible={}", vis);
        vis
    }

    pub fn ensure_ready(app_handle: &AppHandle) -> bool {
        if app_handle.get_webview_window("main").is_none() {
            return false;
        }
        init(app_handle).is_ok()
    }

    pub fn apply_position(
        app_handle: &AppHandle,
        panel_x: f64,
        panel_y: f64,
        _primary_logical_h: f64,
    ) {
        if let Some(window) = app_handle.get_webview_window("main") {
            log::info!(
                "[linux panel] set_position({:.0}, {:.0})",
                panel_x,
                panel_y
            );
            let _ = window
                .set_position(Position::Logical(tauri::LogicalPosition::new(panel_x, panel_y)));
        }
    }
}

// ---------------------------------------------------------------------------
// Cross-platform public API
// ---------------------------------------------------------------------------

pub fn init(app_handle: &AppHandle) -> tauri::Result<()> {
    platform::init(app_handle)
}

/// Retrieve the tray icon rect and position the panel beneath it.
/// On Linux with AppIndicator the tray rect is often unavailable;
/// in that case fall back to the top-right corner of the primary monitor.
fn position_panel_from_tray(app_handle: &AppHandle) {
    let tray_rect = app_handle
        .tray_by_id("tray")
        .and_then(|tray| tray.rect().ok().flatten());

    if let Some(rect) = tray_rect {
        position_panel_at_tray_icon(app_handle, rect.position, rect.size);
        return;
    }

    // Fallback: top-right of primary monitor
    log::debug!("position_panel_from_tray: tray rect unavailable, using top-right fallback");
    let Some(window) = app_handle.get_webview_window("main") else {
        return;
    };
    let Some(monitor) = window.primary_monitor().ok().flatten() else {
        return;
    };
    let scale = monitor.scale_factor();
    let mon_w = monitor.size().width as f64 / scale;
    let panel_width = window
        .outer_size()
        .ok()
        .map(|s| s.width as f64 / scale)
        .unwrap_or(400.0);
    let margin = 8.0;
    let panel_x = mon_w - panel_width - margin;
    let panel_y = margin;
    platform::apply_position(app_handle, panel_x, panel_y, 0.0);
}

/// Show the panel (initializing if needed), positioned under the tray icon.
pub fn show_panel(app_handle: &AppHandle) {
    log::info!("show_panel called");
    platform::show(app_handle);
    position_panel_from_tray(app_handle);
}

/// Hide the panel.
pub fn hide_panel(app_handle: &AppHandle) {
    platform::hide(app_handle);
}

/// Check if the panel is currently visible.
pub fn is_panel_visible(app_handle: &AppHandle) -> bool {
    platform::is_visible(app_handle)
}

/// Toggle panel visibility. Used by global shortcut handler.
pub fn toggle_panel(app_handle: &AppHandle) {
    if !platform::ensure_ready(app_handle) {
        return;
    }

    if platform::is_visible(app_handle) {
        log::debug!("toggle_panel: hiding panel");
        platform::hide(app_handle);
    } else {
        log::debug!("toggle_panel: showing panel");
        platform::show(app_handle);
        position_panel_from_tray(app_handle);
    }
}

pub fn position_panel_at_tray_icon(
    app_handle: &AppHandle,
    icon_position: Position,
    icon_size: Size,
) {
    let window = app_handle.get_webview_window("main").unwrap();

    let (icon_phys_x, icon_phys_y) = match &icon_position {
        Position::Physical(pos) => (pos.x as f64, pos.y as f64),
        Position::Logical(pos) => (pos.x, pos.y),
    };
    let (icon_phys_w, icon_phys_h) = match &icon_size {
        Size::Physical(s) => (s.width as f64, s.height as f64),
        Size::Logical(s) => (s.width, s.height),
    };

    let monitors = window.available_monitors().expect("failed to get monitors");
    let primary_logical_h = window
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| m.size().height as f64 / m.scale_factor())
        .unwrap_or(0.0);

    let icon_center_x = icon_phys_x + (icon_phys_w / 2.0);
    let icon_center_y = icon_phys_y + (icon_phys_h / 2.0);

    let found_monitor = monitors.iter().find(|monitor| {
        let origin = monitor.position();
        let size = monitor.size();
        monitor_contains_physical_point(
            origin.x as f64,
            origin.y as f64,
            size.width as f64,
            size.height as f64,
            icon_center_x,
            icon_center_y,
        )
    });

    let monitor = match found_monitor {
        Some(m) => m.clone(),
        None => {
            log::warn!(
                "No monitor found for tray rect center at ({:.0}, {:.0}), using primary",
                icon_center_x,
                icon_center_y
            );
            match window.primary_monitor() {
                Ok(Some(m)) => m,
                _ => return,
            }
        }
    };

    let target_scale = monitor.scale_factor();
    let mon_phys_x = monitor.position().x as f64;
    let mon_phys_y = monitor.position().y as f64;
    let mon_logical_x = mon_phys_x / target_scale;
    let mon_logical_y = mon_phys_y / target_scale;

    let icon_logical_x = mon_logical_x + (icon_phys_x - mon_phys_x) / target_scale;
    let icon_logical_y = mon_logical_y + (icon_phys_y - mon_phys_y) / target_scale;
    let icon_logical_w = icon_phys_w / target_scale;
    let icon_logical_h = icon_phys_h / target_scale;

    let panel_width = match (window.outer_size(), window.scale_factor()) {
        (Ok(s), Ok(win_scale)) => s.width as f64 / win_scale,
        _ => {
            let conf: serde_json::Value = serde_json::from_str(include_str!("../tauri.conf.json"))
                .expect("tauri.conf.json must be valid JSON");
            conf["app"]["windows"][0]["width"]
                .as_f64()
                .expect("width must be set in tauri.conf.json")
        }
    };

    let icon_center_x = icon_logical_x + (icon_logical_w / 2.0);
    let panel_x = icon_center_x - (panel_width / 2.0);
    let nudge_up: f64 = 6.0;
    let panel_y = icon_logical_y + icon_logical_h - nudge_up;

    platform::apply_position(app_handle, panel_x, panel_y, primary_logical_h);
}
