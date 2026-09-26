// Tauri 全局 API（tauri.conf.json: withGlobalTauri=true）
const T = window.__TAURI__;
const invoke = T && T.core ? T.core.invoke : null;

const $ = (id) => document.getElementById(id);
const home = $('home'), result = $('result');
const hintEl = $('hint'), qrEl = $('qrcode'), urlEl = $('url');
const textInput = $('textInput');

async function start(mode) {
  home.style.display = 'none';
  result.style.display = 'block';
  hintEl.textContent = '正在启动服务器…';
  qrEl.innerHTML = '';
  urlEl.textContent = '';

  if (!invoke) {
    hintEl.textContent = '错误：Tauri API 未注入';
    return;
  }

  try {
    const url = await invoke('start_server', { mode });
    urlEl.textContent = url;
    const svg = await invoke('qr_svg', { text: url });
    qrEl.innerHTML = svg;
    hintEl.textContent = '用另一台设备扫码即可';
  } catch (e) {
    hintEl.textContent = '启动失败: ' + e;
  }
}

// 发送文本：直接读取输入框
$('btnText').addEventListener('click', () => {
  const v = textInput.value.trim();
  if (!v) {
    textInput.focus();
    return;
  }
  start('text:' + v);
});

// 发送文件：弹出系统文件选择器
$('btnFile').addEventListener('click', async () => {
  if (!invoke) return;
  try {
    const path = await invoke('pick_file');
    if (path) start('file:' + path);
  } catch (e) {
    hintEl.textContent = '选择文件失败: ' + e;
  }
});

$('backBtn').addEventListener('click', () => {
  result.style.display = 'none';
  home.style.display = 'block';
});

// 输入框自动增高
textInput.addEventListener('input', () => {
  textInput.style.height = 'auto';
  textInput.style.height = Math.min(textInput.scrollHeight, 200) + 'px';
});

// 底部联系方式切换
const contactLink = $('contactLink');
const qqNumber = $('qqNumber');
contactLink.addEventListener('click', () => {
  qqNumber.classList.toggle('hidden');
});

