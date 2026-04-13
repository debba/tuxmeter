//! XDG Desktop Portal GlobalShortcuts backend for Wayland sessions.
//!
//! On Wayland, `tauri-plugin-global-shortcut` (X11 XGrabKey) is silently
//! ignored by compositors like Mutter. This module uses the
//! `org.freedesktop.portal.GlobalShortcuts` D-Bus interface via `ashpd`.

use std::sync::{Mutex, OnceLock};
use tauri::AppHandle;

const SHORTCUT_ID: &str = "toggle-panel";
const SHORTCUT_DESCRIPTION: &str = "Toggle Tuxmeter panel";

struct PortalState {
    command_tx: tokio::sync::mpsc::UnboundedSender<Option<String>>,
}

fn portal_state_slot() -> &'static Mutex<Option<PortalState>> {
    static SLOT: OnceLock<Mutex<Option<PortalState>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Returns true if the current session is Wayland.
pub fn is_wayland() -> bool {
    if let Ok(session_type) = std::env::var("XDG_SESSION_TYPE") {
        return session_type.eq_ignore_ascii_case("wayland");
    }
    std::env::var("WAYLAND_DISPLAY").is_ok()
}

/// Convert Tauri shortcut format to GTK accelerator format for the portal.
/// e.g. "Control+Shift+U" -> "<Control><Shift>u"
///      "CommandOrControl+Space" -> "<Control>space"
///      "Super+A" -> "<Super>a"
fn to_portal_trigger(tauri_shortcut: &str) -> String {
    let parts: Vec<&str> = tauri_shortcut.split('+').collect();
    let mut result = String::new();
    for (i, part) in parts.iter().enumerate() {
        let is_last = i == parts.len() - 1;
        if is_last {
            // Final key: lowercase
            result.push_str(&part.to_lowercase());
        } else {
            // Modifier: wrap in angle brackets
            let modifier = match *part {
                "CommandOrControl" | "CmdOrCtrl" => "Control",
                "Command" | "Cmd" => "Super",
                other => other,
            };
            result.push('<');
            result.push_str(modifier);
            result.push('>');
        }
    }
    result
}

/// Spawn the background portal listener task.
pub fn spawn_listener(app_handle: AppHandle, initial_shortcut: Option<String>) {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    {
        let mut state = portal_state_slot().lock().unwrap();
        *state = Some(PortalState { command_tx: tx });
    }
    tauri::async_runtime::spawn(portal_task(app_handle, initial_shortcut, rx));
}

/// Update the shortcut binding via the portal.
/// If the listener task is not yet spawned, spawns it.
pub fn update_shortcut(app_handle: &AppHandle, shortcut: Option<String>) -> Result<(), String> {
    let mut state = portal_state_slot().lock().map_err(|e| e.to_string())?;
    if let Some(ref s) = *state {
        if s.command_tx.send(shortcut.clone()).is_ok() {
            return Ok(());
        }
        // Channel dead -- listener exited. Clear state and respawn if needed.
        *state = None;
    }
    drop(state);

    if shortcut.is_some() {
        spawn_listener(app_handle.clone(), shortcut);
    }
    Ok(())
}

async fn bind_shortcut(
    proxy: &ashpd::desktop::global_shortcuts::GlobalShortcuts,
    session: &ashpd::desktop::Session<ashpd::desktop::global_shortcuts::GlobalShortcuts>,
    shortcut: &str,
) {
    use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, NewShortcut};

    let trigger = to_portal_trigger(shortcut);
    eprintln!(
        "[portal_shortcuts] Binding shortcut: {} (trigger: {})",
        shortcut,
        trigger
    );
    let new_shortcut =
        NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION).preferred_trigger(Some(&*trigger));
    match proxy
        .bind_shortcuts(session, &[new_shortcut], None, BindShortcutsOptions::default())
        .await
    {
        Ok(request) => match request.response() {
            Ok(bound) => {
                eprintln!("[portal_shortcuts] Shortcut bound OK: {:?}", bound.shortcuts());
            }
            Err(e) => {
                eprintln!("[portal_shortcuts] Bind response error: {:?}", e);
                // Retry without preferred_trigger
                eprintln!("[portal_shortcuts] Retrying without preferred_trigger...");
                let fallback = NewShortcut::new(SHORTCUT_ID, SHORTCUT_DESCRIPTION);
                match proxy
                    .bind_shortcuts(session, &[fallback], None, BindShortcutsOptions::default())
                    .await
                {
                    Ok(req) => match req.response() {
                        Ok(bound) => eprintln!("[portal_shortcuts] Fallback bound OK: {:?}", bound.shortcuts()),
                        Err(e2) => eprintln!("[portal_shortcuts] Fallback also failed: {:?}", e2),
                    },
                    Err(e2) => eprintln!("[portal_shortcuts] Fallback call failed: {:?}", e2),
                }
            }
        },
        Err(e) => eprintln!("[portal_shortcuts] Failed to bind shortcut: {:?}", e),
    }
}

async fn portal_task(
    app_handle: AppHandle,
    initial_shortcut: Option<String>,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<Option<String>>,
) {
    use ashpd::desktop::global_shortcuts::GlobalShortcuts;
    use ashpd::desktop::CreateSessionOptions;
    use futures_util::StreamExt;

    eprintln!("[portal_shortcuts] Creating proxy...");
    let proxy = match GlobalShortcuts::new().await {
        Ok(p) => {
            eprintln!("[portal_shortcuts] Proxy created OK");
            p
        }
        Err(e) => {
            eprintln!("[portal_shortcuts] Proxy FAILED: {}", e);
            return;
        }
    };

    eprintln!("[portal_shortcuts] Creating session...");
    let session = match proxy.create_session(CreateSessionOptions::default()).await {
        Ok(s) => {
            eprintln!("[portal_shortcuts] Session created OK");
            s
        }
        Err(e) => {
            eprintln!("[portal_shortcuts] Session FAILED: {}", e);
            return;
        }
    };

    if let Some(ref shortcut) = initial_shortcut {
        bind_shortcut(&proxy, &session, shortcut).await;
    }

    let mut activated = match proxy.receive_activated().await {
        Ok(stream) => stream,
        Err(e) => {
            log::warn!("[portal_shortcuts] Failed to get activation stream: {}", e);
            return;
        }
    };

    eprintln!("[portal_shortcuts] Listener running, waiting for activations...");

    loop {
        tokio::select! {
            Some(event) = activated.next() => {
                let id = event.shortcut_id();
                eprintln!("[portal_shortcuts] Activated: {}", id);
                if id == SHORTCUT_ID {
                    crate::panel::toggle_panel(&app_handle);
                }
            }
            Some(command) = rx.recv() => {
                match command {
                    Some(shortcut) => {
                        bind_shortcut(&proxy, &session, &shortcut).await;
                    }
                    None => {
                        log::info!("[portal_shortcuts] Shortcut cleared");
                        // Bind empty list to clear
                        let _ = proxy
                            .bind_shortcuts(
                                &session,
                                &[] as &[ashpd::desktop::global_shortcuts::NewShortcut],
                                None,
                                ashpd::desktop::global_shortcuts::BindShortcutsOptions::default(),
                            )
                            .await;
                        break;
                    }
                }
            }
            else => break,
        }
    }

    log::info!("[portal_shortcuts] Listener ended");
}

#[cfg(test)]
mod tests {
    use super::to_portal_trigger;

    #[test]
    fn converts_control_shift_key() {
        assert_eq!(to_portal_trigger("Control+Shift+U"), "<Control><Shift>u");
    }

    #[test]
    fn converts_command_or_control() {
        assert_eq!(
            to_portal_trigger("CommandOrControl+Space"),
            "<Control>space"
        );
    }

    #[test]
    fn converts_super_modifier() {
        assert_eq!(to_portal_trigger("Super+A"), "<Super>a");
    }

    #[test]
    fn converts_alt_function_key() {
        assert_eq!(to_portal_trigger("Alt+F1"), "<Alt>f1");
    }

    #[test]
    fn converts_multiple_modifiers() {
        assert_eq!(
            to_portal_trigger("Super+Control+A"),
            "<Super><Control>a"
        );
    }
}
