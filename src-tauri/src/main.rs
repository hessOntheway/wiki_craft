use std::fs::{self, OpenOptions};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::thread;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde_json::{Value, json};
use wiki_craft::config::AppConfig;
use wiki_craft::knowledge::WorkspacePaths;

static API_BASE_URL: OnceLock<String> = OnceLock::new();
static GUI_LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

#[tauri::command]
fn get_api_base_url() -> Result<String, String> {
    API_BASE_URL
        .get()
        .cloned()
        .ok_or_else(|| "local API server has not started".to_string())
}

#[tauri::command]
fn log_gui_event(level: String, message: String, context: Option<Value>) -> Result<(), String> {
    let path = GUI_LOG_PATH
        .get()
        .ok_or_else(|| "GUI log path has not been initialized".to_string())?;
    append_gui_log_event(path, &level, &message, context.as_ref()).map_err(|error| {
        eprintln!("warn: failed to append GUI log event: {error:#}");
        format!("{error:#}")
    })
}

#[tauri::command]
fn ingest_local_file(path: String) -> Result<wiki_craft::runtime::IngestOutcome, String> {
    let config_path = wiki_craft::web::config_path_from_env();
    wiki_craft::runtime::run_production_ingest_local_file(&config_path, Path::new(&path))
        .map_err(|error| format!("{error:#}"))
}

fn start_backend() -> anyhow::Result<String> {
    dotenvy::dotenv().ok();

    let config_path = wiki_craft::web::config_path_from_env();
    if let Some(workspace_dir) = config_path.parent() {
        std::env::set_current_dir(workspace_dir).with_context(|| {
            format!(
                "failed to set desktop backend working directory to {}",
                workspace_dir.display()
            )
        })?;
    }
    eprintln!(
        "info: Wiki Craft desktop backend using config {}",
        config_path.display()
    );

    let gui_log_path = gui_log_path_for_config(&config_path)?;
    let _ = GUI_LOG_PATH.set(gui_log_path.clone());
    if let Err(error) = append_gui_log_event(
        &gui_log_path,
        "info",
        "desktop_backend_starting",
        Some(&json!({ "config_path": config_path.display().to_string() })),
    ) {
        eprintln!("warn: failed to append GUI startup log event: {error:#}");
    }

    let listener = TcpListener::bind("127.0.0.1:0")?;
    listener.set_nonblocking(true)?;
    let addr = listener.local_addr()?;
    let api_base_url = format!("http://{addr}");

    thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(error) => {
                eprintln!("error: failed to build desktop web runtime: {error}");
                return;
            }
        };

        runtime.block_on(async move {
            let listener = match tokio::net::TcpListener::from_std(listener) {
                Ok(listener) => listener,
                Err(error) => {
                    eprintln!("error: failed to adopt desktop web listener: {error}");
                    return;
                }
            };
            if let Err(error) = wiki_craft::web::serve_listener(config_path, listener).await {
                eprintln!("error: desktop API server exited: {error:#}");
            }
        });
    });

    Ok(api_base_url)
}

fn gui_log_path_for_config(config_path: &Path) -> anyhow::Result<PathBuf> {
    let config = AppConfig::load_or_default(config_path).with_context(|| {
        format!(
            "failed to load config for GUI log path: {}",
            config_path.display()
        )
    })?;
    let paths = WorkspacePaths::from_config(&config);
    Ok(paths.root.join("runtime").join("gui").join("events.jsonl"))
}

fn append_gui_log_event(
    path: &Path,
    level: &str,
    message: &str,
    context: Option<&Value>,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create GUI log dir: {}", parent.display()))?;
    }
    let event = json!({
        "kind": "gui_event",
        "ts_unix_ms": unix_ms(),
        "level": level,
        "message": message,
        "context": context.cloned().unwrap_or(Value::Null),
    });
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open GUI log: {}", path.display()))?;
    writeln!(file, "{event}").context("failed to append GUI log event")
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|_app| {
            let api_base_url = start_backend().map_err(|error| {
                eprintln!("error: failed to start desktop backend: {error:#}");
                Box::<dyn std::error::Error>::from(error)
            })?;
            let _ = API_BASE_URL.set(api_base_url);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            get_api_base_url,
            log_gui_event,
            ingest_local_file
        ])
        .run(tauri::generate_context!())
        .expect("error while running Wiki Craft desktop app");
}
