use anyhow::Result;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use base64::Engine;
use std::{
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
                 <title>SoFast</title></head>\
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
/// 普通路径用标准文件系统读取；content URI 通过 JNI 调用 Android ContentResolver，
/// 避免受 fs 插件 scope 权限限制导致「读取文件失败」。
async fn read_source(_app: tauri::AppHandle, source: &str) -> Result<(Vec<u8>, String)> {
    // content:// URI：走 Android ContentResolver
    if source.starts_with("content://") {
        let src_for_name = source.to_string();
        let filename = tokio::task::spawn_blocking(move || {
            content_uri_filename(&src_for_name).unwrap_or_else(|| "file".to_string())
        })
        .await?;

        let src = source.to_string();
        let bytes = tokio::task::spawn_blocking(move || read_content_uri(&src)).await??;
        return Ok((bytes, filename));
    }

    // 普通路径 / file://
    let filename = std::path::Path::new(source.strip_prefix("file://").unwrap_or(source))
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let path = source.strip_prefix("file://").unwrap_or(source).to_string();
    let bytes = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        Ok(std::fs::read(&path)?)
    })
    .await??;

    Ok((bytes, filename))
}

/// 通过 Android ContentResolver 读取 content:// URI 的完整字节。
#[cfg(target_os = "android")]
fn read_content_uri(uri: &str) -> Result<Vec<u8>> {
    use jni::objects::{JObject, JValue};

    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }?;
    let mut env = vm.attach_current_thread()?;

    let uri_str = env.new_string(uri)?;
    let uri_obj = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValue::Object(&uri_str)],
        )?
        .l()?;

    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let resolver = env
        .call_method(
            &activity,
            "getContentResolver",
            "()Landroid/content/ContentResolver;",
            &[],
        )?
        .l()?;

    let stream = env
        .call_method(
            &resolver,
            "openInputStream",
            "(Landroid/net/Uri;)Ljava/io/InputStream;",
            &[JValue::Object(&uri_obj)],
        )?
        .l()?;

    if stream.is_null() {
        return Err(anyhow::anyhow!("无法打开文件流（URI 可能无效或权限不足）"));
    }

    let bytes = read_stream_all(&mut env, &stream)?;
    let _ = env.call_method(&stream, "close", "()V", &[]);
    Ok(bytes)
}

/// 循环读取 InputStream 全部字节（兼容所有 Android 版本）。
#[cfg(target_os = "android")]
fn read_stream_all(env: &mut jni::JNIEnv, stream: &jni::objects::JObject) -> Result<Vec<u8>> {
    use jni::objects::JValue;

    let buffer = env.new_byte_array(8192)?;
    let mut acc: Vec<u8> = Vec::new();
    loop {
        let n = env
            .call_method(stream, "read", "([B)I", &[JValue::Object(&buffer)])?
            .i()?;
        if n <= 0 {
            break;
        }
        // 先把整个 buffer 转成 Vec<u8>，再取前 n 个字节
        let full: Vec<u8> = env.convert_byte_array(&buffer)?;
        acc.extend_from_slice(&full[..n as usize]);
    }
    Ok(acc)
}

/// 从 content:// URI 查询文件名（OpenableColumns.DISPLAY_NAME）。
#[cfg(target_os = "android")]
fn content_uri_filename(uri: &str) -> Option<String> {
    use jni::objects::{JObject, JValue};

    let ctx = ndk_context::android_context();
    let vm = unsafe { jni::JavaVM::from_raw(ctx.vm().cast()) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;

    let uri_str = env.new_string(uri).ok()?;
    let uri_obj = env
        .call_static_method(
            "android/net/Uri",
            "parse",
            "(Ljava/lang/String;)Landroid/net/Uri;",
            &[JValue::Object(&uri_str)],
        )
        .ok()?
        .l()
        .ok()?;

    let activity = unsafe { JObject::from_raw(ctx.context().cast()) };
    let resolver = env
        .call_method(
            &activity,
            "getContentResolver",
            "()Landroid/content/ContentResolver;",
            &[],
        )
        .ok()?
        .l()
        .ok()?;

    // String[] projection = { OpenableColumns.DISPLAY_NAME }
    let display_name = env.new_string("_display_name").ok()?;
    let projection = env
        .new_object_array(1, "java/lang/String", &display_name)
        .ok()?;

    let cursor = env
        .call_method(
            &resolver,
            "query",
            "(Landroid/net/Uri;[Ljava/lang/String;Ljava/lang/String;[Ljava/lang/String;Ljava/lang/String;)Landroid/database/Cursor;",
            &[
                JValue::Object(&uri_obj),
                JValue::Object(&projection),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
                JValue::Object(&JObject::null()),
            ],
        )
        .ok()?
        .l()
        .ok()?;

    if cursor.is_null() {
        return None;
    }

    let moved = env
        .call_method(&cursor, "moveToFirst", "()Z", &[])
        .ok()?
        .z()
        .ok()?;

    let name = if moved {
        let col = env.new_string("_display_name").ok()?;
        let idx = env
            .call_method(
                &cursor,
                "getColumnIndex",
                "(Ljava/lang/String;)I",
                &[JValue::Object(&col)],
            )
            .ok()?
            .i()
            .ok()?;
        if idx >= 0 {
            let s = env
                .call_method(&cursor, "getString", "(I)Ljava/lang/String;", &[JValue::Int(idx)])
                .ok()?
                .l()
                .ok()?;
            if s.is_null() {
                None
            } else {
                let js: jni::objects::JString = s.into();
                env.get_string(&js).ok().map(|g| g.into())
            }
        } else {
            None
        }
    } else {
        None
    };

    let _ = env.call_method(&cursor, "close", "()V", &[]);
    name
}

#[cfg(not(target_os = "android"))]
fn read_content_uri(uri: &str) -> Result<Vec<u8>> {
    Err(anyhow::anyhow!("当前平台不支持 content:// URI: {}", uri))
}

#[cfg(not(target_os = "android"))]
fn content_uri_filename(_uri: &str) -> Option<String> {
    None
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
