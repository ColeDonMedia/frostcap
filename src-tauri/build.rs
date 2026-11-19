/// Every app command, registered in an app manifest so none is callable unless a
/// capability grants `allow-<command>` (see capabilities/default.json).
const COMMANDS: &[&str] = &[
    "list_sources",
    "list_cameras",
    "pick_region",
    "start_recording",
    "pause_recording",
    "resume_recording",
    "stop_recording",
    "recording_state",
    "load_project",
    "save_project",
    "list_recent_projects",
    "recover_project",
    "scrub_project",
    "generate_auto_zoom",
    "export_project",
    "cancel_export",
    "pick_project",
    "pick_folder",
    "pick_save_path",
    "reveal_in_folder",
    "hotkeys",
    "set_hotkeys",
    "default_output_dir",
    "launch_options",
    "export_batch",
    "encoder_capabilities",
    "suggest_cuts",
    "pick_image",
    "import_background",
    "write_selftest_report",
    "selftest_cleanup",
    "capture_settings",
    "set_capture_border",
    "set_exclude_own_windows",
];

fn main() {
    println!("cargo:rerun-if-changed=windows-app-manifest.xml");
    // Windows resource (icon, version info, manifest with per-monitor-v2 DPI
    // awareness + common controls v6) and the ACL/context data.
    tauri_build::try_build(
        tauri_build::Attributes::new()
            .app_manifest(tauri_build::AppManifest::new().commands(COMMANDS))
            .windows_attributes(
                tauri_build::WindowsAttributes::new()
                    .app_manifest(include_str!("windows-app-manifest.xml")),
            ),
    )
    .expect("tauri build");
}
