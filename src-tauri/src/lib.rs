use reqwest::Method;
use serde::{Deserialize, Serialize};
use tauri_plugin_sql::{Migration, MigrationKind};

#[derive(Serialize)]
struct HttpResponse {
    status: u16,
    duration_ms: u64,
    body: String,
    headers: Vec<(String, String)>,
    size_bytes: u64,
}

#[derive(Deserialize)]
struct QueryParam {
    key: String,
    value: String,
    #[serde(default = "default_true")]
    encode: bool,
}

fn default_true() -> bool {
    true
}

fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char);
            }
            _ => {
                out.push('%');
                out.push_str(&format!("{:02X}", b));
            }
        }
    }
    out
}

fn build_url(base: &str, params: &[QueryParam]) -> String {
    let active: Vec<&QueryParam> = params.iter().filter(|p| !p.key.is_empty()).collect();
    if active.is_empty() {
        return base.to_string();
    }
    let mut s = String::from(base);
    let mut first = !base.contains('?');
    for p in active {
        s.push(if first { '?' } else { '&' });
        first = false;
        s.push_str(&encode_uri_component(&p.key));
        s.push('=');
        if p.encode {
            s.push_str(&encode_uri_component(&p.value));
        } else {
            s.push_str(&p.value);
        }
    }
    s
}

#[tauri::command]
async fn send_request(
    method: String,
    url: String,
    params: Option<Vec<QueryParam>>,
) -> Result<HttpResponse, String> {
    let method = Method::from_bytes(method.to_uppercase().as_bytes())
        .map_err(|e| format!("invalid method: {e}"))?;

    let final_url = match params.as_deref() {
        Some(p) if !p.is_empty() => build_url(&url, p),
        _ => url,
    };

    let client = reqwest::Client::builder()
        .user_agent("vento/0.1")
        .build()
        .map_err(|e| e.to_string())?;

    let started = std::time::Instant::now();
    let resp = client
        .request(method, &final_url)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let body = resp.text().await.map_err(|e| e.to_string())?;
    let size_bytes = body.as_bytes().len() as u64;
    let duration_ms = started.elapsed().as_millis() as u64;

    Ok(HttpResponse { status, duration_ms, body, headers, size_bytes })
}

const SECRET_SERVICE: &str = "com.vento.env";

fn keyring_entry(env_id: i64, key: &str) -> Result<keyring::Entry, String> {
    keyring::Entry::new(SECRET_SERVICE, &format!("{env_id}::{key}"))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn secret_set(env_id: i64, key: String, value: String) -> Result<(), String> {
    keyring_entry(env_id, &key)?
        .set_password(&value)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn secret_get(env_id: i64, key: String) -> Result<Option<String>, String> {
    match keyring_entry(env_id, &key)?.get_password() {
        Ok(s) => Ok(Some(s)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

#[tauri::command]
fn secret_delete(env_id: i64, key: String) -> Result<(), String> {
    match keyring_entry(env_id, &key)?.delete_credential() {
        Ok(_) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let migrations = vec![
        Migration {
            version: 1,
            description: "create_history",
            sql: "CREATE TABLE IF NOT EXISTS history (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            method TEXT NOT NULL,
            url TEXT NOT NULL,
            status INTEGER,
            duration_ms INTEGER,
            response_preview TEXT,
            sent_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_history_sent_at ON history(sent_at DESC);",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 2,
            description: "create_collections",
            sql: "CREATE TABLE IF NOT EXISTS collections (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS folders (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
                parent_folder_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_folders_collection ON folders(collection_id);
            CREATE INDEX IF NOT EXISTS idx_folders_parent ON folders(parent_folder_id);
            CREATE TABLE IF NOT EXISTS saved_requests (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE,
                folder_id INTEGER REFERENCES folders(id) ON DELETE CASCADE,
                name TEXT NOT NULL,
                method TEXT NOT NULL,
                url TEXT NOT NULL,
                created_at INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_requests_collection ON saved_requests(collection_id);
            CREATE INDEX IF NOT EXISTS idx_requests_folder ON saved_requests(folder_id);",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 3,
            description: "add_params_column",
            sql: "ALTER TABLE history ADD COLUMN params TEXT;
            ALTER TABLE saved_requests ADD COLUMN params TEXT;",
            kind: MigrationKind::Up,
        },
        Migration {
            version: 4,
            description: "create_environments",
            sql: "CREATE TABLE IF NOT EXISTS environments (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT NOT NULL,
                is_globals INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL
            );
            CREATE UNIQUE INDEX IF NOT EXISTS idx_env_globals_singleton
                ON environments(is_globals) WHERE is_globals = 1;
            CREATE TABLE IF NOT EXISTS env_variables (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                environment_id INTEGER NOT NULL REFERENCES environments(id) ON DELETE CASCADE,
                key TEXT NOT NULL,
                value TEXT NOT NULL DEFAULT '',
                secret INTEGER NOT NULL DEFAULT 0,
                enabled INTEGER NOT NULL DEFAULT 1,
                position INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_env_vars_env ON env_variables(environment_id);
            INSERT INTO environments (name, is_globals, created_at)
                SELECT 'Globals', 1, strftime('%s','now')*1000
                WHERE NOT EXISTS (SELECT 1 FROM environments WHERE is_globals = 1);",
            kind: MigrationKind::Up,
        },
    ];

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(
            tauri_plugin_sql::Builder::default()
                .add_migrations("sqlite:vento.db", migrations)
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            send_request,
            secret_set,
            secret_get,
            secret_delete
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
