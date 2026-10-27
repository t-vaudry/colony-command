// Links go to the default browser. A click, a target=_blank, and the context
// menu's "Open link in new window" would otherwise open them inside the app.

use tauri::webview::NewWindowResponse;
use tauri::{App, AppHandle, Url, WebviewWindow, WebviewWindowBuilder};
use tauri_plugin_opener::OpenerExt;

/// Builds the main window (tauri.conf.json has `create: false`) with link handling attached.
pub fn main_window(app: &App) -> tauri::Result<WebviewWindow> {
    let config = app.config().app.windows.iter().find(|w| w.label == "main").expect("main window config");
    let nav = app.handle().clone();
    let new = app.handle().clone();
    WebviewWindowBuilder::from_config(app.handle(), config)?
        .on_navigation(move |url| {
            if is_app(url) {
                return true;
            }
            open(&nav, url);
            false
        })
        .on_new_window(move |url, _| {
            open(&new, &url);
            NewWindowResponse::Deny
        })
        .build()
}

/// The app's own pages: the bundled frontend, or the Vite dev server.
fn is_app(url: &Url) -> bool {
    match url.scheme() {
        "tauri" | "about" | "data" | "blob" => true,
        "http" | "https" => match url.host_str() {
            Some("tauri.localhost") => true,
            Some("localhost") => tauri::is_dev() && url.port() == Some(5173),
            _ => false,
        },
        _ => false,
    }
}

/// For links the page itself handles (the terminal pane), which never navigate.
#[tauri::command]
pub fn open_link(app: AppHandle, url: String) {
    if let Ok(url) = Url::parse(&url) {
        open(&app, &url);
    }
}

fn open(app: &AppHandle, url: &Url) {
    // Only web links: a page shouldn't be able to launch file: or app: handlers.
    if matches!(url.scheme(), "http" | "https" | "mailto") {
        if let Err(e) = app.opener().open_url(url.as_str(), None::<&str>) {
            eprintln!("colony: open {url}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_app_pages_stay_in_the_window() {
        let url = |s: &str| Url::parse(s).unwrap();
        assert!(is_app(&url("http://tauri.localhost/index.html")));
        assert!(is_app(&url("tauri://localhost/")));
        assert!(!is_app(&url("https://github.com/t-vaudry/colony-command")));
        assert!(!is_app(&url("http://localhost:3000/")));
    }
}
