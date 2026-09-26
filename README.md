# sf —— 扫码在手机和电脑之间传东西

一个局域网内通过**二维码**在手机与电脑之间互传文件/文本的小工具。

## 三个产物

| 产物 | 平台 | 界面 | 功能 |
|------|------|------|------|
| `sf` | Linux | 命令行 | 终端打印二维码，见下文三种用法 |
| `sf.exe` | Windows | 图形界面 | 双击打开窗口，输入文字 / 选文件 / 接收 |
| `sf.apk` | Android | 图形界面 | 手机 App，发送文本/文件，别人扫码下载 |

## 命令行 `sf`（Linux）用法

```bash
sf "消息"           # 终端显示二维码，手机扫码后文本自动进手机剪贴板
sf /path/to/file    # 终端显示二维码，手机扫码即可下载该文件
sf get              # 终端显示二维码，手机扫码后可把手机上的文本/文件发到电脑
```

- `sf get` 模式下，电脑端会把收到的文本复制到系统剪贴板（依次尝试 `wl-copy` / `xclip` / `xsel`），文件则保存到当前目录。
- 按 `Ctrl+C` 退出；`sf get` 收到内容后自动退出。

## 项目结构

```
sf-mobile/
├── sf-cli/                 # Linux 命令行版（纯 CLI）
│   ├── Cargo.toml
│   └── src/main.rs         # 入口（HTTP server + 终端二维码）
├── src-tauri/              # Tauri 桌面版 + 移动版（Windows exe / Android apk）
│   ├── Cargo.toml
│   ├── src/
│   │   ├── main.rs         # 桌面入口
│   │   ├── lib.rs          # Tauri command（start_server / qr_svg / pick_file）
│   │   └── server.rs       # HTTP server + 二维码 + 文件读取
│   └── gen/android/        # Tauri 生成的 Android 工程
├── src/                    # 前端（index.html / main.js / styles.css）
└── releases/               # 构建产物
    ├── sf                  # Linux CLI
    ├── sf.exe              # Windows GUI
    └── sf.apk              # Android（arm64-v8a）
```

## 构建命令

### 1. Linux CLI（`sf`）

```bash
cd sf-cli
cargo build --release
# 产物: sf-cli/target/release/sf
```

### 2. Windows exe（`sf.exe`）

在 Linux 上交叉编译（需安装 `mingw-w64` 和 rust target `x86_64-pc-windows-gnu`）：

```bash
rustup target add x86_64-pc-windows-gnu
cd src-tauri
RUSTFLAGS="-C link-args=-Wl,--exclude-all-symbols" \
  cargo build --release --target x86_64-pc-windows-gnu
# 产物: src-tauri/target/x86_64-pc-windows-gnu/release/sf-mobile.exe
```

> **⚠️ 关键踩坑**：`RUSTFLAGS="-C link-args=-Wl,--exclude-all-symbols"` **必须加**。
> 否则新版 mingw-w64（`ld` 2.46+）会因 crate-type 里的 `cdylib` 导出符号过多而报：
> `error: export ordinal too large: 67xxx`。

### 3. Android APK（`sf.apk`）

```bash
# 只构建 arm64-v8a（现代手机通用，体积最小 ~8MB）
npm run tauri android build --target aarch64 --apk
# 产物: src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk
```

> **⚠️ 体积说明**：不加 `--target aarch64` 会把 4 个 ABI（arm64-v8a / armeabi-v7a / x86 / x86_64）
> 全打进一个 universal APK，体积会涨到 ~25MB。手机只需 arm64-v8a，所以**务必加 `--target aarch64`**。

### 4. 拷贝到 releases/

```bash
cp sf-cli/target/release/sf releases/sf
cp src-tauri/target/x86_64-pc-windows-gnu/release/sf-mobile.exe releases/sf.exe
cp src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk releases/sf.apk
```

## Android 文件读取说明

Android 上通过文件选择器拿到的是 `content://` URI，无法直接用标准文件系统打开。
项目通过 **JNI 调用 Android `ContentResolver.openInputStream()`** 读取（见 `src-tauri/src/server.rs`
中的 `read_content_uri`），避免受 `tauri-plugin-fs` 的 scope 权限限制。

对应依赖（仅 Android 目标）：
```toml
[target.'cfg(target_os = "android")'.dependencies]
jni = "0.21"
ndk-context = "0.1"
```

## 环境要求

- Rust（含 `x86_64-pc-windows-gnu` target）
- `mingw-w64`（交叉编译 Windows）
- Node.js + `@tauri-apps/cli`
- Android SDK + NDK（构建 APK，需设置 `ANDROID_HOME` / `NDK_HOME`）
