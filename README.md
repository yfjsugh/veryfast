# sf-mobile

把手机上的文本或文件，通过局域网 + 二维码快速传到另一台设备。

## 功能

- **发送文本**：在输入框输入文本，点「发送文本」→ 生成二维码，另一台设备扫码即可看到文本。
- **发送文件**：点「发送文件」→ 系统文件选择器选文件 → 生成二维码，另一台设备扫码即可下载。

## 工作原理

1. 前端（WebView）调用 Rust 命令 `start_server`。
2. Rust 起一个 axum HTTP 服务，监听 `0.0.0.0` 的随机端口，并返回本机局域网地址。
3. 前端把该地址渲染成二维码（Rust 端 `qr_svg` 生成 SVG）。
4. 另一台设备扫码访问 → 服务端返回内容：
   - 文本模式：直接展示文本页面。
   - 文件模式：返回带 base64 下载链接的页面。

### 关键实现点

- **单服务生命周期**：全局持有当前服务的 shutdown 句柄，每次 `start_server` 会先停掉上一个服务，避免端口泄漏。
- **Android content URI**：系统文件选择器（`tauri-plugin-dialog`）在 Android 上返回 `content://` URI，普通 `std::fs` 读不了。使用 `tauri-plugin-fs` 的 `Fs::open`，其 Android 实现会通过 ContentResolver 拿到文件描述符（fd），Rust 侧即可当作普通文件读取。
- **前端不依赖打包器**：`tauri.conf.json` 的 `withGlobalTauri = true`，前端直接使用 `window.__TAURI__.core.invoke`，无需 ESM 打包。

## 目录结构

```
src/                    前端（HTML/CSS/JS）
  index.html            主页 + 结果页
  main.js               调用 Tauri 命令、渲染二维码
  styles.css
src-tauri/
  src/lib.rs            Tauri 入口，注册插件与命令
  src/server.rs         HTTP 服务、二维码生成、文件读取
  capabilities/         权限（dialog / fs / opener）
```

## 构建

```bash
cargo tauri android build --target aarch64 --apk
```

产物：`src-tauri/gen/android/app/build/outputs/apk/universal/release/app-universal-release.apk`

## 开发说明

- **不要修改 `src-tauri/gen/android/` 下的生成文件**（除 `MainActivity.kt` 等模板允许修改的部分），会被重新生成覆盖。
- 前端只用了原生 HTML/CSS/JS，没有引入构建工具，改动 `src/` 后直接重新构建 APK 即可。
