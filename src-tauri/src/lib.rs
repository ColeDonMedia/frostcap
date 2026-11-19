//! Frostcap desktop app (Tauri v2 shell). All capture/project logic lives in
//! `frostcap-core`; this crate wires it to the webview ([`backend`]), defines the
//! wire DTOs ([`dto`]) and the filesystem path policy ([`paths`]).

pub mod backend;
pub mod dto;
pub mod paths;

use tauri::{Manager, RunEvent, WindowEvent};

/// Build and run the app.
pub fn run() {
    let app = tauri::Builder::default()
        .manage(backend::AppState::default())
        .invoke_handler(tauri::generate_handler![
            backend::list_sources,
            backend::list_cameras,
            backend::pick_region,
            backend::start_recording,
            backend::pause_recording,
            backend::resume_recording,
            backend::stop_recording,
            backend::recording_state,
            backend::load_project,
            backend::save_project,
            backend::list_recent_projects,
            backend::recover_project,
            backend::scrub_project,
            backend::generate_auto_zoom,
            backend::export_project,
            backend::cancel_export,
            backend::pick_project,
            backend::pick_folder,
            backend::pick_save_path,
            backend::reveal_in_folder,
            backend::hotkeys,
            backend::set_hotkeys,
            backend::default_output_dir,
            backend::launch_options,
            backend::export_batch,
            backend::encoder_capabilities,
            backend::suggest_cuts,
            backend::pick_image,
            backend::import_background,
            backend::write_selftest_report,
            backend::selftest_cleanup,
            backend::capture_settings,
            backend::set_capture_border,
            backend::set_exclude_own_windows,
        ])
        .setup(|app| {
            frostcap_core::recorder::ensure_dpi_awareness();
            let handle = app.handle().clone();
            let state = app.state::<backend::AppState>();
            if let Err(e) = state.init(&handle) {
                eprintln!("frostcap: path policy init failed: {}", e.message);
            }
            if let Some(window) = app.get_webview_window("main") {
                apply_backdrop(&window);
            }
            // Load D3D11 / Media Foundation / encoder DLLs so the first recording starts fast.
            if let Ok(root) = handle.path().video_dir().map(|v| v.join("Frostcap")) {
                std::thread::spawn(move || {
                    let _ = frostcap_core::recorder::prewarm(&root);
                });
            }
            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window while recording finalizes the recording first.
            if let WindowEvent::CloseRequested { .. } = event {
                let app = window.app_handle();
                let _ = backend::stop_active(app, &app.state::<backend::AppState>());
            }
        })
        .build(tauri::generate_context!());
    let app = match app {
        Ok(a) => a,
        Err(e) => {
            eprintln!("frostcap failed to start: {e}");
            std::process::exit(1);
        }
    };
    app.run(|app, event| {
        if let RunEvent::ExitRequested { .. } | RunEvent::Exit = event {
            let _ = backend::stop_active(app, &app.state::<backend::AppState>());
            frostcap_export::release_gpu();
        }
    });
}

/// Mica on Windows 11; silently keep the plain transparent window elsewhere.
fn apply_backdrop(window: &tauri::WebviewWindow) {
    #[cfg(windows)]
    {
        let _ = window_vibrancy::apply_mica(window, None);
    }
    #[cfg(not(windows))]
    let _ = window;
}

#[cfg(test)]
mod tests {
    /// The embedded Windows manifest must declare Per-Monitor-V2 DPI awareness
    /// (physical-pixel cursor coordinates) and keep Common Controls v6.
    #[test]
    fn windows_manifest_declares_per_monitor_v2() {
        let m = include_str!("../windows-app-manifest.xml");
        assert!(m.contains(">PerMonitorV2</dpiAwareness>"));
        assert!(m.contains(">true/pm</dpiAware>"));
        assert!(m.contains("Microsoft.Windows.Common-Controls") && m.contains("6.0.0.0"));
        assert!(!m.contains("requireAdministrator"));
    }
}
