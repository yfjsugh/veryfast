use anyhow::Result;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use base64::Engine;
use std::{
    io::Read,
    net::{IpAddr, UdpSocket},
    sync::Arc,
};
use tokio::sync::Mutex;

/// 全局只保留一个正在运行的服务，便于新请求到来时先停掉旧的。
static CURRENT: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::const_new(None);

#[derive(Clone)]
pub struct ServerState {
    pub app: tauri::AppHandle,
    pub mode: Arc<Mode>,
}

pub enum Mode {
    Text(String),
    /// 文件来源，可能是普通文件路径，也可能是 Android content:// URI
    File(String),
}

pub async fn start(app: tauri::AppHandle, mode_str: String) -> Result<String> {
    // 先停掉上一个服务（如果有）
    {
        let mut guard = CURRENT.lock().await;
        if let Some(tx) = guard.take() {
            let _ = tx.send(());
        }
    }

    let mode = parse_mode(&mode_str);
    let state = ServerState {
        app,
        mode: Arc::new(mode),
    };

    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0u16)).await?;
    let port = listener.local_addr()?.port();
    let ip = local_ip().unwrap_or(IpAddr::from([127, 0, 0, 1]));
    let url = format!("http://{}:{}/", ip, port);

    let router = Router::new()
        .route("/", get(root_handler))
        .with_state(state);

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    {
        let mut guard = CURRENT.lock().await;
        *guard = Some(tx);
    }

    tokio::spawn(async move {
        let _ = axum::serve(listener, router)
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });

    Ok(url)
}

async fn root_handler(State(state): State<ServerState>) -> impl IntoResponse {
    let html = match &*state.mode {
        Mode::Text(text) => {
            let safe = html_escape(text);
            format!(
                "<!DOCTYPE html><html><head><meta charset='UTF-8'>\
                 <meta name='viewport' content='width=device-width,initial-scale=1'>\
                 <title>sf</title></head>\
                 <body style='text-align:center;padding:40px;font-family:sans-serif;word-break:break-all'>\
                 <h2>收到文本</h2><p style='font-size:20px'>{}</p></body></html>",
                safe
            )
        }
        Mode::File(source) => match read_source(state.app.clone(), source).await {
            Ok((bytes, filename)) => {
                let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
                let safe = html_escape(&filename);
                format!(
                    "<!DOCTYPE html><html><head><meta charset='UTF-8'>\
                     <meta name='viewport' content='width=device-width,initial-scale=1'>\
                     <title>{}</title></head>\
                     <body style='text-align:center;padding:40px;font-family:sans-serif'>\
                     <h2>文件已就绪</h2><p>{}</p>\
                     <a href='data:application/octet-stream;base64,{}' download='{}'>\
                     点击下载</a></body></html>",
                    safe, safe, encoded, safe
                )
            }
            Err(e) => format!(
                "<!DOCTYPE html><html><head><meta charset='UTF-8'></head>\
                 <body style='text-align:center;padding:40px;font-family:sans-serif'>\
                 <h2>文件读取失败</h2><p style='color:#999;font-size:12px'>{}</p></body></html>",
                html_escape(&e.to_string())
            ),
        },
    };
    Html(html)
}

/// 读取文件内容与文件名，兼容普通路径与 Android content:// URI。
/// 普通路径用标准文件系统读取；content URI 交给 fs 插件（内部经 ContentResolver 打开）。
async fn read_source(app: tauri::AppHandle, source: &str) -> Result<(Vec<u8>, String)> {
    use tauri::Manager;
    use tauri_plugin_fs::{FilePath, FsExt};

    // 文件名
    let filename = if source.starts_with("content://") {
        app.path()
            .file_name(source)
            .unwrap_or_else(|| "file".to_string())
    } else {
        std::path::Path::new(source)
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string())
    };

    // 构造 FilePath：content:// / file:// 用 Url，其余用 Path
    let file_path = if source.contains("://") {
        FilePath::Url(url::Url::parse(source)?)
    } else {
        FilePath::Path(std::path::PathBuf::from(source))
    };

    // 文件 IO 是阻塞的，放到阻塞线程
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut opts = tauri_plugin_fs::OpenOptions::new();
        opts.read(true);
        let mut file = app
            .fs()
            .open(file_path, opts)
            .map_err(anyhow::Error::from)?;
        let mut buf = Vec::new();
        file.read_to_end(&mut buf)?;
        Ok(buf)
    })
    .await??;

    Ok((bytes, filename))
}

/// 简单 HTML 转义，避免用户输入破坏页面结构
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn local_ip() -> Option<IpAddr> {
    for target in ["8.8.8.8:80", "1.1.1.1:80", "223.5.5.5:80"] {
        if let Ok(sock) = UdpSocket::bind("0.0.0.0:0") {
            if sock.connect(target).is_ok() {
                if let Ok(addr) = sock.local_addr() {
                    let ip = addr.ip();
                    if !ip.is_loopback() && !ip.is_unspecified() {
                        return Some(ip);
                    }
                }
            }
        }
    }
    None
}

fn parse_mode(s: &str) -> Mode {
    if let Some(text) = s.strip_prefix("text:") {
        Mode::Text(text.to_string())
    } else if let Some(path) = s.strip_prefix("file:") {
        Mode::File(path.to_string())
    } else {
        Mode::Text(s.to_string())
    }
}

/// 生成二维码的 SVG 字符串，供前端直接嵌入显示
pub fn qr_svg(text: &str) -> Result<String> {
    use qrcode::render::svg;
    let code = qrcode::QrCode::new(text.as_bytes())?;
    let svg = code
        .render::<svg::Color>()
        .min_dimensions(240, 240)
        .dark_color(svg::Color("#000000"))
        .light_color(svg::Color("#ffffff"))
        .build();
    Ok(svg)
}
