//! Speech to text for the message boxes. The webview records 16 kHz mono audio and
//! sends it here; whisper.cpp transcribes it on this machine, so nothing leaves it.
//! The model (~150 MB) is downloaded once, the first time the user turns dictation on.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde::Serialize;
use tauri::ipc::{InvokeBody, Request};
use tauri::{AppHandle, Emitter, Manager, State};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

const MODEL_FILE: &str = "ggml-base.en.bin";
const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
/// A truncated download is far below this; the real file is ~148 MB.
const MODEL_MIN_BYTES: u64 = 100 * 1024 * 1024;
const SAMPLE_RATE: usize = 16_000;
/// Whisper needs at least a second of audio; shorter clips come back as hallucinated filler.
const MIN_SAMPLES: usize = SAMPLE_RATE / 2;

#[derive(Default)]
pub struct Speech {
    ctx: Mutex<Option<Arc<WhisperContext>>>,
    downloading: Mutex<bool>,
}

#[derive(Serialize)]
pub struct Status {
    ready: bool,
    model_mb: u64,
}

fn model_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?.join("models");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Can't create {}: {e}", dir.display()))?;
    Ok(dir.join(MODEL_FILE))
}

fn model_ready(path: &PathBuf) -> bool {
    std::fs::metadata(path).map(|m| m.len() >= MODEL_MIN_BYTES).unwrap_or(false)
}

#[tauri::command]
pub fn speech_status(app: AppHandle) -> Result<Status, String> {
    Ok(Status { ready: model_ready(&model_path(&app)?), model_mb: 148 })
}

/// Downloads the model, emitting `speech-download` with the percent done (0-100).
#[tauri::command]
pub async fn speech_download(app: AppHandle, state: State<'_, Speech>) -> Result<(), String> {
    let path = model_path(&app)?;
    if model_ready(&path) {
        return Ok(());
    }
    {
        let mut busy = state.downloading.lock().unwrap_or_else(|e| e.into_inner());
        if *busy {
            return Err("The speech model is already downloading.".into());
        }
        *busy = true;
    }
    let handle = app.clone();
    let result = tauri::async_runtime::spawn_blocking(move || download(&handle, &path)).await.map_err(|e| e.to_string());
    *state.downloading.lock().unwrap_or_else(|e| e.into_inner()) = false;
    result?
}

fn download(app: &AppHandle, path: &PathBuf) -> Result<(), String> {
    let resp = ureq::get(MODEL_URL).call().map_err(|e| format!("Couldn't download the speech model: {e}"))?;
    let total = resp.body().content_length().unwrap_or(0);
    let part = path.with_extension("part");
    let mut out = std::fs::File::create(&part).map_err(|e| e.to_string())?;
    let mut reader = resp.into_body().into_reader();
    let (mut buf, mut done, mut last) = (vec![0u8; 256 * 1024], 0u64, 101u64);
    loop {
        let n = reader.read(&mut buf).map_err(|e| format!("Download interrupted: {e}"))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        let pct = if total > 0 { done * 100 / total } else { 0 };
        if pct != last {
            last = pct;
            let _ = app.emit("speech-download", pct);
        }
    }
    drop(out);
    if done < MODEL_MIN_BYTES || (total > 0 && done != total) {
        let _ = std::fs::remove_file(&part);
        return Err("The speech model download was incomplete. Try again.".into());
    }
    std::fs::rename(&part, path).map_err(|e| e.to_string())
}

fn context(app: &AppHandle, state: &Speech) -> Result<Arc<WhisperContext>, String> {
    let mut slot = state.ctx.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = slot.as_ref() {
        return Ok(c.clone());
    }
    let path = model_path(app)?;
    if !model_ready(&path) {
        return Err("The speech model isn't downloaded yet.".into());
    }
    let c = WhisperContext::new_with_params(&path, WhisperContextParameters::default()).map_err(|e| {
        // A corrupt file would fail every time; drop it so the next try downloads afresh.
        let _ = std::fs::remove_file(&path);
        format!("Couldn't load the speech model: {e}")
    })?;
    let c = Arc::new(c);
    *slot = Some(c.clone());
    Ok(c)
}

/// 16-bit little-endian mono PCM at 16 kHz in the raw request body; returns the text.
#[tauri::command]
pub async fn speech_transcribe(app: AppHandle, request: Request<'_>) -> Result<String, String> {
    let InvokeBody::Raw(bytes) = request.body() else {
        return Err("Expected raw audio bytes.".into());
    };
    let samples: Vec<f32> = bytes.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.0).collect();
    if samples.len() < MIN_SAMPLES {
        return Ok(String::new());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<Speech>();
        let ctx = context(&app, &state)?;
        let mut st = ctx.create_state().map_err(|e| e.to_string())?;
        let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
        params.set_language(Some("en"));
        params.set_n_threads(std::thread::available_parallelism().map(|n| n.get().min(8) as i32).unwrap_or(4));
        params.set_translate(false);
        params.set_no_context(true);
        params.set_print_special(false);
        params.set_print_progress(false);
        params.set_print_realtime(false);
        params.set_print_timestamps(false);
        st.full(params, &samples).map_err(|e| format!("Transcription failed: {e}"))?;
        let text: String = st.as_iter().filter_map(|s| s.to_str().ok().map(str::to_owned)).collect::<Vec<_>>().join(" ");
        Ok(clean(&text))
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Whisper marks silence and noise with bracketed tags like `[BLANK_AUDIO]` or `(music)`.
fn clean(text: &str) -> String {
    let mut out = String::new();
    let mut depth = 0u32;
    for ch in text.chars() {
        match ch {
            '[' | '(' => depth += 1,
            ']' | ')' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::clean;

    #[test]
    fn strips_noise_tags() {
        assert_eq!(clean(" [BLANK_AUDIO]"), "");
        assert_eq!(clean(" Fix the bug (music) in  main.rs [pause]"), "Fix the bug in main.rs");
    }
}
