use anyhow::{Context, Result};
use axum::{
    body::Body,
    extract::{Multipart, State},
    http::{header, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
    Router,
};
use qrcode::{render::unicode, QrCode};
use std::{
    io::Write,
    net::{IpAddr, UdpSocket},
    path::{Path, PathBuf},
    process::{Command as ProcCommand, Stdio},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Notify;
use tokio_util::io::ReaderStream;

// ───────────────────────── 模式 & 状态 ─────────────────────────

enum Mode {
    /// sf "message" —— 把文本推到手机剪贴板
    Text(String),
    /// sf <file> —— 让手机下载文件
    File(PathBuf),
    /// sf get —— 从手机接收文本/文件
    Get,
}

#[derive(Clone)]
struct AppState {
    mode: Arc<Mode>,
    done: Arc<Notify>,
}

// ───────────────────────── main ─────────────────────────

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mode = parse_args(&args);
    let is_get = matches!(mode, Mode::Get);

    let state = AppState {
        mode: Arc::new(mode),
        done: Arc::new(Notify::new()),
    };

    let app = Router::new()
        .route("/", get(root))
        .route("/upload", post(upload))
        .with_state(state.clone());

    // 端口交给系统随机分配
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", 0u16))
        .await
        .context("无法绑定端口")?;
    let port = listener.local_addr()?.port();

    let ip = local_ip().unwrap_or(IpAddr::from([127, 0, 0, 1]));
    let url = format!("http://{}:{}/", ip, port);

    // 打印二维码
    println!();
    print_qr(&url);
    println!();
    println!("  🌐 {}", url);
    println!();
    match &*state.mode {
        Mode::Text(_) => println!("  📱 用手机扫码，文本会自动复制到手机剪贴板"),
        Mode::File(p) => println!("  📱 用手机扫码下载: {}", p.display()),
        Mode::Get => println!("  📱 用手机扫码，粘贴文本或选择文件发送到电脑"),
    }
    println!("  按 Ctrl+C 退出");
    println!();

    let srv_state = state.clone();
    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("服务器错误: {e}");
            srv_state.done.notify_one();
        }
    });

    if is_get {
        state.done.notified().await;
        // 等一下，让 HTTP 响应先发回手机
        tokio::time::sleep(Duration::from_millis(700)).await;
        println!("\n✅ 已收到内容，退出。");
    } else {
        tokio::signal::ctrl_c().await?;
        println!("\n👋 退出。");
    }

    Ok(())
}

fn parse_args(args: &[String]) -> Mode {
    if args.is_empty() {
        eprintln!("sf —— 用二维码在手机和电脑之间传东西\n");
        eprintln!("用法:");
        eprintln!("  sf \"消息\"        扫码后把文本复制到手机剪贴板");
        eprintln!("  sf <文件路径>     扫码后下载文件到手机");
        eprintln!("  sf get            扫码后把手机上的文本/文件发送到电脑");
        std::process::exit(2);
    }
    if args.len() == 1 {
        let a = &args[0];
        if a == "get" {
            return Mode::Get;
        }
        let p = PathBuf::from(a);
        if p.is_file() {
            return Mode::File(p);
        }
    }
    Mode::Text(args.join(" "))
}

// ───────────────────────── 网络工具 ─────────────────────────

/// 拿到局域网 IP：向公网地址 connect 一个 UDP socket（不发包），
/// 内核会帮我们选出默认出口网卡的地址。
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

/// 终端二维码（Unicode 半块字符）
fn print_qr(url: &str) {
    match QrCode::new(url.as_bytes()) {
        Ok(code) => {
            // 注意：深色模块用「亮色字符」绘制 —— 适配绝大多数深色终端
            let s = code
                .render::<unicode::Dense1x2>()
                .quiet_zone(true)
                .dark_color(unicode::Dense1x2::Light)
                .light_color(unicode::Dense1x2::Dark)
                .build();
            println!("{}", s);
        }
        Err(e) => eprintln!("生成二维码失败: {e}"),
    }
}

// ───────────────────────── HTTP handlers ─────────────────────────

async fn root(State(state): State<AppState>) -> Response {
    match &*state.mode {
        Mode::Text(t) => Html(text_page(t)).into_response(),
        Mode::Get => Html(get_page()).into_response(),
        Mode::File(p) => match serve_file(p).await {
            Ok(resp) => resp,
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("读取文件失败: {e}"),
            )
                .into_response(),
        },
    }
}

async fn serve_file(path: &Path) -> Result<Response> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("打开文件失败: {}", path.display()))?;
    let len = file.metadata().await?.len();

    let name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "download".to_string());

    // 流式返回，大文件也不会吃满内存
    let body = Body::from_stream(ReaderStream::new(file));

    let resp = Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/octet-stream")
        .header(header::CONTENT_LENGTH, len)
        .header(header::CONTENT_DISPOSITION, content_disposition(&name))
        .body(body)?;

    Ok(resp)
}

/// RFC 6266 / RFC 5987：同时给出 ASCII fallback 和 UTF-8 编码的文件名
fn content_disposition(filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii() && c != '"' && c != '\\' && !c.is_control() {
                c
            } else {
                '_'
            }
        })
        .collect();

    let mut encoded = String::new();
    for b in filename.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{:02X}", b));
        }
    }

    format!(
        "attachment; filename=\"{}\"; filename*=UTF-8''{}",
        ascii, encoded
    )
}

async fn upload(State(state): State<AppState>, mut multipart: Multipart) -> Response {
    let mut notes: Vec<String> = Vec::new();
    let mut got_something = false;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (StatusCode::BAD_REQUEST, format!("解析上传失败: {e}")).into_response();
            }
        };

        let name = field.name().unwrap_or("").to_string();
        let filename = field.file_name().map(|s| s.to_string());

        match filename {
            Some(fname) if !fname.is_empty() => {
                let data = match field.bytes().await {
                    Ok(d) => d,
                    Err(e) => {
                        return (StatusCode::BAD_REQUEST, format!("读取文件失败: {e}"))
                            .into_response()
                    }
                };
                if data.is_empty() {
                    continue;
                }
                let path = unique_path(&fname);
                match tokio::fs::write(&path, &data).await {
                    Ok(()) => {
                        let shown = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
                        println!("📁 已保存文件: {} ({} 字节)", shown.display(), data.len());
                        notes.push(format!("文件已保存到 {}", shown.display()));
                        got_something = true;
                    }
                    Err(e) => notes.push(format!("保存文件失败: {e}")),
                }
            }
            _ if name == "text" => {
                let text = match field.text().await {
                    Ok(t) => t,
                    Err(e) => {
                        return (StatusCode::BAD_REQUEST, format!("读取文本失败: {e}"))
                            .into_response()
                    }
                };
                if text.trim().is_empty() {
                    continue;
                }
                println!("📝 收到文本:");
                println!("{}", text);
                println!();

                let ok = copy_to_clipboard(&text);
                notes.push(if ok {
                    "文本已复制到电脑剪贴板".to_string()
                } else {
                    "文本已打印在电脑终端（未找到剪贴板工具）".to_string()
                });
                got_something = true;
            }
            _ => {}
        }
    }

    if !got_something {
        return Html(result_page("没有收到任何内容", &[])).into_response();
    }

    // 通知主任务可以退出了
    state.done.notify_one();

    let refs: Vec<&str> = notes.iter().map(|s| s.as_str()).collect();
    Html(result_page("发送成功", &refs)).into_response()
}

// ───────────────────────── 工具函数 ─────────────────────────

/// 往系统剪贴板写文本。依次尝试 wl-copy / xclip / xsel。
fn copy_to_clipboard(text: &str) -> bool {
    let candidates: [(&str, Vec<&str>); 3] = [
        ("wl-copy", vec![]),
        ("xclip", vec!["-selection", "clipboard"]),
        ("xsel", vec!["--clipboard", "--input"]),
    ];

    for (cmd, args) in candidates {
        let Ok(mut child) = ProcCommand::new(cmd)
            .args(&args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        else {
            continue;
        };

        if let Some(mut stdin) = child.stdin.take() {
            if stdin.write_all(text.as_bytes()).is_err() {
                continue;
            }
            let _ = stdin.flush();
        }
        // 不 wait：xclip/xsel 需要常驻才能持有剪贴板内容
        return true;
    }
    false
}

/// 保证落盘时文件名不冲突
fn unique_path(filename: &str) -> PathBuf {
    let safe = Path::new(filename)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();

    let safe = if safe.is_empty() || safe == "." || safe == ".." {
        "file".to_string()
    } else {
        safe
    };

    let p = PathBuf::from(&safe);
    if !p.exists() {
        return p;
    }

    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".to_string());
    let ext = p.extension().map(|s| s.to_string_lossy().into_owned());

    for i in 1..10_000 {
        let name = match &ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let q = PathBuf::from(&name);
        if !q.exists() {
            return q;
        }
    }

    PathBuf::from(format!("{stem}-{}", std::process::id()))
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

// ───────────────────────── HTML 模板 ─────────────────────────

const TEXT_TPL: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>sf · 文本</title>
<style>
  *{box-sizing:border-box}
  body{margin:0;padding:20px;background:#111;color:#eee;
       font-family:system-ui,-apple-system,"PingFang SC","Microsoft YaHei",sans-serif}
  .wrap{max-width:640px;margin:0 auto}
  textarea{width:100%;height:40vh;padding:12px;font-size:16px;line-height:1.5;
           border-radius:10px;border:1px solid #444;background:#1c1c1c;color:#eee;resize:vertical}
  button{margin-top:12px;width:100%;padding:15px;font-size:17px;border:0;border-radius:10px;
         background:#3b82f6;color:#fff;font-weight:600}
  button:active{background:#2563eb}
  .ok{margin-top:10px;text-align:center;color:#4ade80;height:1.3em;font-size:15px}
</style>
</head>
<body>
<div class="wrap">
  <textarea id="t" readonly>__TEXT__</textarea>
  <button id="b">复制到剪贴板</button>
  <div class="ok" id="s"></div>
</div>
<script>
var ta = document.getElementById('t');
var st = document.getElementById('s');

function fallbackCopy(){
  ta.removeAttribute('readonly');
  ta.focus();
  ta.select();
  ta.setSelectionRange(0, ta.value.length);
  var ok = false;
  try { ok = document.execCommand('copy'); } catch(e) {}
  ta.setAttribute('readonly','readonly');
  return ok;
}

async function doCopy(){
  var ok = false;
  // HTTP + 局域网 IP 不是 secure context，navigator.clipboard 通常不可用
  try {
    if (navigator.clipboard && window.isSecureContext) {
      await navigator.clipboard.writeText(ta.value);
      ok = true;
    }
  } catch(e) {}
  if (!ok) ok = fallbackCopy();
  st.textContent = ok ? '✓ 已复制到剪贴板' : '✗ 复制失败，请长按文本手动复制';
  st.style.color = ok ? '#4ade80' : '#f87171';
}

document.getElementById('b').addEventListener('click', doCopy);
window.addEventListener('load', doCopy);
</script>
</body>
</html>"#;

const GET_TPL: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>sf · 发送到电脑</title>
<style>
  *{box-sizing:border-box}
  body{margin:0;padding:20px;background:#111;color:#eee;
       font-family:system-ui,-apple-system,"PingFang SC","Microsoft YaHei",sans-serif}
  .wrap{max-width:640px;margin:0 auto}
  h1{font-size:20px;margin:0 0 16px}
  textarea{width:100%;height:30vh;padding:12px;font-size:16px;line-height:1.5;
           border-radius:10px;border:1px solid #444;background:#1c1c1c;color:#eee;resize:vertical}
  .file{margin:16px 0;padding:18px;border:1px dashed #555;border-radius:10px;text-align:center}
  .file .tip{margin-bottom:10px;color:#aaa;font-size:14px}
  input[type=file]{width:100%;color:#eee;font-size:15px}
  button{margin-top:8px;width:100%;padding:15px;font-size:17px;border:0;border-radius:10px;
         background:#3b82f6;color:#fff;font-weight:600}
  button:active{background:#2563eb}
  button:disabled{background:#555}
</style>
</head>
<body>
<div class="wrap">
  <h1>发送到电脑</h1>
  <form id="f" method="POST" action="/upload" enctype="multipart/form-data">
    <textarea name="text" placeholder="在这里粘贴文本…"></textarea>
    <div class="file">
      <div class="tip">或者选择一个文件</div>
      <input type="file" name="file">
    </div>
    <button type="submit" id="b">发送</button>
  </form>
</div>
<script>
document.getElementById('f').addEventListener('submit', function(){
  var b = document.getElementById('b');
  b.disabled = true;
  b.textContent = '发送中…';
});
</script>
</body>
</html>"#;

const RESULT_TPL: &str = r#"<!DOCTYPE html>
<html lang="zh">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>sf · 完成</title>
<style>
  body{margin:0;padding:40px 20px;background:#111;color:#eee;text-align:center;
       font-family:system-ui,-apple-system,"PingFang SC","Microsoft YaHei",sans-serif}
  .big{font-size:52px;margin-bottom:8px}
  h1{font-size:20px;margin:0 0 20px}
  p{color:#aaa;font-size:15px;word-break:break-all;margin:6px 0}
</style>
</head>
<body>
  <div class="big">✅</div>
  <h1>__TITLE__</h1>
  __BODY__
</body>
</html>"#;

fn text_page(text: &str) -> String {
    TEXT_TPL.replace("__TEXT__", &html_escape(text))
}

fn get_page() -> String {
    GET_TPL.to_string()
}

fn result_page(title: &str, lines: &[&str]) -> String {
    let body: String = lines
        .iter()
        .map(|l| format!("<p>{}</p>", html_escape(l)))
        .collect();
    RESULT_TPL
        .replace("__TITLE__", &html_escape(title))
        .replace("__BODY__", &body)
}
