// No console window behind the map in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod daemon;

/// The map calls this to connect; starts colonyd if it isn't running.
#[tauri::command]
async fn daemon_info() -> Result<daemon::Info, String> {
    tauri::async_runtime::spawn_blocking(daemon::ensure).await.map_err(|e| e.to_string())?
}

fn main() {
    tauri::Builder::default()
        .setup(|_| {
            // Start the daemon while the window loads rather than on first ask.
            std::thread::spawn(|| {
                if let Err(e) = daemon::ensure() {
                    eprintln!("colony: {e}");
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![daemon_info])
        .run(tauri::generate_context!())
        .expect("Colony Command failed to start");
}
