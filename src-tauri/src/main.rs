#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod open;
mod scan;

#[tauri::command]
async fn load_projects() -> Vec<scan::Project> {
    tauri::async_runtime::spawn_blocking(scan::scan_all).await.unwrap_or_default()
}

#[tauri::command]
fn detect_apps() -> Vec<open::App> {
    open::detect()
}

#[tauri::command]
fn missing_clis() -> Vec<&'static str> {
    open::missing_clis()
}

#[tauri::command]
fn open_in(app: String, dir: String, harness: Option<String>, session: Option<String>) -> Result<(), String> {
    open::open_in(&app, &dir, harness.as_deref(), session.as_deref())
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![load_projects, detect_apps, missing_clis, open_in])
        .run(tauri::generate_context!())
        .expect("error while running SeshDock");
}
