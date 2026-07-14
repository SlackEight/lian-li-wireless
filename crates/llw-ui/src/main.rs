#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[tauri::command]
fn ping() -> &'static str {
    "pong"
}

fn main() {
    // webkit2gtk's DMABUF renderer crashes with "Error 71 (Protocol error)"
    // on Wayland + NVIDIA (observed on the dev machine, KDE Plasma 6 + RTX
    // 5090, 2026-07-14). Disable it before GTK initializes.
    if std::env::var_os("WAYLAND_DISPLAY").is_some()
        && std::env::var_os("WEBKIT_DISABLE_DMABUF_RENDERER").is_none()
    {
        std::env::set_var("WEBKIT_DISABLE_DMABUF_RENDERER", "1");
    }

    tauri::Builder::default()
        .invoke_handler(tauri::generate_handler![ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
