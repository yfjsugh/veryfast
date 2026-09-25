mod server;

use tauri_plugin_dialog::DialogExt;

#[tauri::command]
async fn start_server(app_handle: tauri::AppHandle, mode: String) -> Result<String, String> {
    server::start(app_handle, mode)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn qr_svg(text: String) -> Result<String, String> {
    server::qr_svg(&text).map_err(|e| e.to_string())
}

/// 弹出系统文件选择器，返回所选文件的路径（取消则返回 None）
#[tauri::command]
async fn pick_file(app: tauri::AppHandle) -> Result<Option<String>, String> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    app.dialog()
        .file()
        .pick_file(move |path| {
            let _ = tx.send(path);
        });
    match rx.await {
        Ok(Some(p)) => Ok(Some(p.to_string())),
        Ok(None) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![start_server, qr_svg, pick_file])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
