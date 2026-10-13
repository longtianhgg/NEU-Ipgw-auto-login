'use strict';
const $ = id => document.getElementById(id);
const state = { settings: null, account: { username: '', has_password: false }, status: {}, logs: { items: [] }, connected: false, page: 'home', logPage: 1, localBusy: false };
const dirty = { account: false, appearance: false, advanced: false };
let toastTimer, passwordTimer, revealedPassword = null;
const systemTheme = window.matchMedia('(prefers-color-scheme: dark)');
const dateTime = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
const clock = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false });
const icon = name => `<svg aria-hidden="true"><use href="#i-${name}"/></svg>`;
const escapeHtml = value => String(value ?? '').replace(/[&<>"']/g, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

function toast(message, error = false) {
  clearTimeout(toastTimer);
  $('toast').textContent = message;
  $('toast').classList.toggle('error', error);
  $('toast').hidden = false;
  toastTimer = setTimeout(() => { $('toast').hidden = true; }, error ? 8000 : 4500);
}

async function api(path, options = {}) {
  const headers = { 'X-NEU-Request': '1', ...options.headers };
  let body = options.body;
  if (body !== undefined && !(body instanceof FormData)) { headers['Content-Type'] = 'application/json'; body = JSON.stringify(body); }
  let response;
  try { response = await fetch(path, { ...options, headers, body, cache: 'no-store' }); }
  catch { throw new Error('无法连接本机服务，请检查服务是否正在运行'); }
  const result = await response.json().catch(() => null);
  if (!response.ok) throw new Error(result?.error || `请求失败（HTTP ${response.status}）`);
  return result;
}

function applyAppearance(settings) {
  if (!settings) return;
  const theme = settings.theme === 'system' ? (systemTheme.matches ? 'dark' : 'light') : settings.theme;
  document.body.dataset.theme = theme;
  document.documentElement.style.setProperty('--panel-alpha', String((theme === 'dark' ? 0.18 : 0.08) + settings.opacity * 0.004));
  document.documentElement.style.setProperty('--blur', `${settings.blur}px`);
  const image = settings.background === 'default' ? '/assets/background.svg' : `/api/backgrounds/${encodeURIComponent(settings.background)}`;
  document.documentElement.style.setProperty('--wallpaper', `url("${image}")`);
}

function fillForms() {
  if (!state.settings) return;
  if (!dirty.account) {
    $('username').value = state.account.username;
    $('password').value = '';
    $('password').required = !state.account.has_password;
    $('password').placeholder = state.account.has_password ? '留空则保留已保存的密码' : '请输入统一身份认证密码';
    $('password-help').textContent = state.account.has_password ? '修改时留空则保留原密码。密码保存后不会自动登录。' : '首次保存需要填写密码。';
  }
  if (!dirty.appearance) {
    const radio = document.querySelector(`input[name="theme"][value="${state.settings.theme}"]`);
    if (radio) radio.checked = true;
    $('opacity').value = state.settings.opacity;
    $('blur').value = state.settings.blur;
    $('opacity-output').textContent = `${state.settings.opacity}%`;
    $('blur-output').textContent = `${state.settings.blur} px`;
  }
  if (!dirty.advanced) {
    $('auto-refresh').checked = state.settings.auto_refresh;
    $('refresh-interval').value = state.settings.refresh_interval_seconds;
    $('request-timeout').value = state.settings.timeout_seconds;
    $('auth-mode').value = state.settings.auth_mode;
    $('cas-url').value = state.settings.cas_login_url;
    $('ipgw-url').value = state.settings.ipgw_base_url;
    $('ac-id').value = state.settings.ac_id;
    $('rsa-key').value = state.settings.rsa_public_key;
    customAuthVisibility();
  }
}

function customAuthVisibility() {
  const custom = $('auth-mode').value === 'custom';
  $('custom-auth').hidden = !custom;
  for (const id of ['cas-url', 'ipgw-url', 'ac-id', 'rsa-key']) { $(id).required = custom; $(id).disabled = !custom; }
}

function hidePassword() {
  clearTimeout(passwordTimer);
  revealedPassword = null;
  $('home-password').textContent = state.account.has_password ? '••••••••••••' : '尚未设置';
  $('reveal-password').setAttribute('aria-pressed', 'false');
  $('reveal-password').setAttribute('aria-label', '显示已保存密码');
}

function renderDuration() {
  const status = state.status;
  if (status.state !== 'online' || !status.online_since) { $('stat-duration').textContent = '—'; return; }
  const seconds = Math.max(0, Math.floor(Date.now() / 1000) - status.online_since);
  const hours = Math.floor(seconds / 3600);
  $('stat-duration').textContent = `${hours ? hours + ':' : ''}${String(Math.floor(seconds % 3600 / 60)).padStart(2, '0')}:${String(seconds % 60).padStart(2, '0')}`;
}

function renderLogs(target, items) {
  if (!items.length) { target.innerHTML = '<p class="empty-state">还没有操作记录，连接校园网后会显示在这里。</p>'; return; }
  const titles = { login: '校园网登录', logout: '退出校园网', account: '账号设置', settings: '应用设置', background: '更换背景', status: '状态检测' };
  target.innerHTML = items.map(log => {
    const neutral = !['login', 'logout', 'status'].includes(log.action);
    const logIcon = neutral ? 'settings' : log.action === 'logout' ? 'logout' : log.success ? 'check' : 'info';
    return `<div class="log-row"><span class="log-icon ${neutral ? 'neutral' : log.success ? '' : 'failed'}">${icon(logIcon)}</span><div class="log-copy"><strong>${escapeHtml(titles[log.action] || '操作记录')}</strong><p>${escapeHtml(log.message)}</p></div><time datetime="${new Date(log.time * 1000).toISOString()}">${escapeHtml(dateTime.format(new Date(log.time * 1000)))}</time></div>`;
  }).join('');
}

function renderHome() {
  const status = state.status;
  const type = ['online', 'offline'].includes(status.state) ? status.state : 'unknown';
  $('status-dot').className = `status-dot ${type}`;
  $('connection-orb').className = `connection-orb ${type}`;
  $('connection-title').textContent = type === 'online' ? '已连接' : type === 'offline' ? '未连接' : status.checked_at ? '检测失败' : '待检测';
  $('connection-message').textContent = status.message || '正在获取校园网状态';
  $('stat-account').textContent = (type === 'online' ? status.username : null) || state.account.username || '未设置';
  $('home-account').textContent = state.account.username || '尚未设置';
  $('home-auth').textContent = state.settings?.auth_mode === 'custom' ? '自定义认证地址' : '自动（默认配置）';
  $('home-auth').title = state.settings?.auth_mode === 'custom' ? state.settings.ipgw_base_url : 'https://ipgw.neu.edu.cn';
  if (revealedPassword === null) hidePassword();
  $('reveal-password').disabled = !state.account.has_password;
  $('stat-check').textContent = status.checked_at ? clock.format(new Date(status.checked_at * 1000)) : '—';
  $('duration-label').textContent = status.duration_source === 'observed' ? '已观察在线时长' : '已在线时长';
  $('network-detail').textContent = status.ip ? `网络出口 ${status.ip} · ${status.duration_source === 'observed' ? '时长从本服务首次检测到在线开始计算' : '状态来自校园网认证服务器'}` : '认证的是运行此服务的设备所使用的网络出口';
  if (state.settings) $('refresh-summary').textContent = state.settings.auto_refresh ? `当前间隔 ${state.settings.refresh_interval_seconds} 秒` : '定时检测已关闭';
  const busy = status.busy || (state.localBusy ? 'request' : null);
  $('busy-label').hidden = !busy;
  $('busy-label').textContent = ({ login: '正在认证，请稍候…', logout: '正在注销，请稍候…', refresh: '正在检测…', request: '正在提交操作…' })[busy] || '操作中…';
  document.querySelectorAll('.operation-button').forEach(button => { button.disabled = Boolean(busy) || !state.connected; });
  $('refresh-button').querySelector('svg').classList.toggle('spinning', busy === 'refresh');
  renderDuration();
  renderLogs($('recent-logs'), state.logs.items.slice(0, 3));
}

function serviceConnection(connected) {
  state.connected = connected;
  $('service-dot').classList.toggle('connected', connected);
  $('service-label').textContent = connected ? '本机服务已连接' : '连接中断 · 正在重连';
  renderHome();
}

function acceptSnapshot(snapshot) {
  const settingsChanged = JSON.stringify(state.settings) !== JSON.stringify(snapshot.settings);
  const accountChanged = JSON.stringify(state.account) !== JSON.stringify(snapshot.account);
  state.settings = snapshot.settings;
  state.account = snapshot.account;
  state.status = snapshot.status;
  state.logs = snapshot.logs;
  if (accountChanged) hidePassword();
  if (settingsChanged && !dirty.appearance) applyAppearance(state.settings);
  if (settingsChanged || accountChanged) fillForms();
  renderHome();
  if ($('logs-dialog').open && state.logPage === 1) renderLogPage(snapshot.logs);
}

function navigate(page) {
  if (!['home', 'account', 'appearance', 'advanced'].includes(page)) page = 'home';
  state.page = page;
  hidePassword();
  const titles = { home: '连接，让生活更简单', account: '账号设置', appearance: '一点风景，一点自己的风格', advanced: '让连接，按你的节奏' };
  $('page-title').textContent = titles[page];
  document.querySelectorAll('.page').forEach(section => { section.hidden = section.id !== `page-${page}`; section.classList.toggle('active', !section.hidden); });
  document.querySelectorAll('.nav-item').forEach(button => { const active = button.dataset.page === page; button.classList.toggle('active', active); if (active) button.setAttribute('aria-current', 'page'); else button.removeAttribute('aria-current'); });
  if (page !== 'appearance') applyAppearance(state.settings);
  history.replaceState(null, '', `#${page}`);
}

async function operation(action) {
  if (!state.account.has_password && action === 'login') { navigate('account'); toast('请先填写并保存账号密码'); $('username').focus(); return; }
  state.localBusy = true; renderHome();
  try {
    const result = await api(action === 'refresh' ? '/api/status/refresh' : `/api/${action}`, { method: 'POST' });
    state.status = action === 'refresh' ? result : result.status;
    toast(action === 'refresh' ? result.state === 'unknown' ? result.message : '校园网状态已更新' : result.message, action === 'refresh' && result.state === 'unknown');
  } catch (error) { toast(error.message, true); }
  finally { state.localBusy = false; renderHome(); }
}

async function saveSettings(patch, message) {
  if (!state.settings) throw new Error('设置仍在加载，请稍候再试');
  const settings = await api('/api/settings', { method: 'PUT', body: { ...state.settings, ...patch } });
  state.settings = settings;
  applyAppearance(settings);
  fillForms();
  renderHome();
  toast(message);
  return settings;
}

async function formAction(form, fn) {
  const buttons = [...form.querySelectorAll('button')];
  buttons.forEach(button => { button.disabled = true; });
  try { await fn(); } catch (error) { toast(error.message, true); }
  finally { buttons.forEach(button => { button.disabled = false; }); }
}

function previewAppearance() {
  dirty.appearance = true;
  const preview = { ...state.settings, theme: document.querySelector('input[name="theme"]:checked').value, opacity: Number($('opacity').value), blur: Number($('blur').value) };
  $('opacity-output').textContent = `${preview.opacity}%`;
  $('blur-output').textContent = `${preview.blur} px`;
  applyAppearance(preview);
}

async function loadBackgrounds() {
  const response = await api('/api/backgrounds');
  $('library-hint').textContent = response.library_configured ? '包含本地上传和服务器图片目录中的图片。' : '尚未配置服务器图片目录，可使用本地上传。';
  const gallery = $('background-gallery'); gallery.replaceChildren();
  for (const background of response.items) {
    const button = document.createElement('button');
    button.type = 'button'; button.className = `background-tile ${background.id === state.settings.background ? 'selected' : ''}`;
    button.setAttribute('aria-pressed', String(background.id === state.settings.background));
    const image = document.createElement('img'); image.src = background.url; image.alt = ''; image.loading = 'lazy';
    const label = document.createElement('span'); label.textContent = background.name;
    const source = document.createElement('small'); source.textContent = ({ upload: '本地上传', library: '服务器图片', builtin: '默认背景' })[background.source];
    label.append(source); button.append(image, label);
    button.addEventListener('click', async () => {
      button.disabled = true;
      try { await saveSettings({ background: background.id }, '背景已更换'); await loadBackgrounds(); }
      catch (error) { toast(error.message, true); }
      finally { button.disabled = false; }
    });
    gallery.append(button);
  }
}

async function openBackgrounds() {
  hidePassword(); $('background-dialog').showModal();
  $('background-gallery').innerHTML = '<p class="empty-state">正在加载图片…</p>';
  try { await loadBackgrounds(); } catch (error) { $('background-gallery').replaceChildren(); toast(error.message, true); }
}

function renderLogPage(page) {
  state.logPage = page.page;
  renderLogs($('full-logs'), page.items);
  const pages = Math.max(1, Math.ceil(page.total / page.page_size));
  $('logs-page').textContent = `第 ${page.page} / ${pages} 页 · 共 ${page.total} 条`;
  $('logs-prev').disabled = page.page <= 1;
  $('logs-next').disabled = page.page >= pages;
}
async function loadLogs(page) { try { renderLogPage(await api(`/api/logs?page=${page}`)); } catch (error) { toast(error.message, true); } }

document.querySelectorAll('[data-page]').forEach(button => button.addEventListener('click', () => navigate(button.dataset.page)));
document.querySelector('.brand').addEventListener('click', event => { event.preventDefault(); navigate('home'); });
window.addEventListener('hashchange', () => navigate(location.hash.slice(1)));
$('refresh-button').addEventListener('click', () => operation('refresh'));
$('login-button').addEventListener('click', () => operation('login'));
$('logout-button').addEventListener('click', () => operation('logout'));
$('account-form').addEventListener('input', () => { dirty.account = true; });
$('account-form').addEventListener('submit', event => {
  event.preventDefault();
  formAction(event.currentTarget, async () => {
    state.account = await api('/api/account', { method: 'PUT', body: { username: $('username').value, password: $('password').value || null } });
    $('password').value = ''; $('password').type = 'password'; dirty.account = false; hidePassword(); fillForms(); renderHome(); toast('账号已保存，可返回首页登录校园网');
  });
});
$('input-password-toggle').addEventListener('click', () => { const showing = $('password').type === 'password'; $('password').type = showing ? 'text' : 'password'; $('input-password-toggle').setAttribute('aria-label', showing ? '隐藏输入的密码' : '显示输入的密码'); });
$('reveal-password').addEventListener('click', async () => {
  if (revealedPassword !== null) { hidePassword(); return; }
  $('reveal-password').disabled = true;
  try { const result = await api('/api/account/reveal', { method: 'POST' }); revealedPassword = result.password; $('home-password').textContent = revealedPassword; $('reveal-password').setAttribute('aria-pressed', 'true'); $('reveal-password').setAttribute('aria-label', '隐藏已保存密码'); passwordTimer = setTimeout(hidePassword, 30000); }
  catch (error) { toast(error.message, true); }
  finally { $('reveal-password').disabled = !state.account.has_password; }
});
document.addEventListener('visibilitychange', () => { if (document.hidden) hidePassword(); });
window.addEventListener('pagehide', hidePassword);
$('appearance-form').addEventListener('input', previewAppearance);
$('appearance-form').addEventListener('submit', event => {
  event.preventDefault();
  formAction(event.currentTarget, async () => {
    await saveSettings({ theme: document.querySelector('input[name="theme"]:checked').value, opacity: Number($('opacity').value), blur: Number($('blur').value) }, '外观设置已保存');
    dirty.appearance = false; fillForms();
  });
});
$('appearance-reset').addEventListener('click', () => formAction($('appearance-form'), async () => { await saveSettings({ theme: 'system', opacity: 65, blur: 18, background: 'default' }, '已恢复默认外观'); dirty.appearance = false; fillForms(); }));
$('advanced-form').addEventListener('input', () => { dirty.advanced = true; });
$('auth-mode').addEventListener('change', customAuthVisibility);
$('advanced-form').addEventListener('submit', event => {
  event.preventDefault();
  formAction(event.currentTarget, async () => {
    const auth = $('auth-mode').value === 'custom' ? { auth_mode: 'custom', cas_login_url: $('cas-url').value, ipgw_base_url: $('ipgw-url').value, ac_id: Number($('ac-id').value), rsa_public_key: $('rsa-key').value } : { auth_mode: 'auto' };
    await saveSettings({ auto_refresh: $('auto-refresh').checked, refresh_interval_seconds: Number($('refresh-interval').value), timeout_seconds: Number($('request-timeout').value), ...auth }, '高级设置已保存');
    dirty.advanced = false; fillForms();
  });
});
$('advanced-reset').addEventListener('click', () => formAction($('advanced-form'), async () => {
  await saveSettings({ auth_mode: 'auto', auto_refresh: true, refresh_interval_seconds: 15, timeout_seconds: 12, cas_login_url: 'https://pass.neu.edu.cn/tpass/login', ipgw_base_url: 'https://ipgw.neu.edu.cn', ac_id: 1, rsa_public_key: defaultPublicKey }, '已恢复默认检测和认证设置'); dirty.advanced = false; fillForms();
}));
const defaultPublicKey = 'MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAnjA28DLKXZzxbKmo9/1WkVLf1mr+wtLXLXt6sC4WiBCtsbzF5ewm7ARZeAdS3iZtqlYPn6IcUoOw42H8nAK/tfFcIb6dZ1K0atn0U39oWCGPzYuKtLJeMuNZiDXVuAXtojrckOjLW9B3gUnaNGLuIx0fYe66l0o9WjU2cGLNZQfiIxs2h00z1EA9IdSnVxiVQWSD+lsP3JZXh2TT287la4Y4603SQNKTK/QvXfcmccwTEd1IW6HwGxD6QrkInBiHisKWxmveN7UDSaQRZ/J97G0YC32pD38WT53izXeK0p/kU/X37VP555um1wVWFvPIuc9I7gMP1+hq5a+X6c++tQIDAQAB';
$('background-button').addEventListener('click', openBackgrounds);
$('appearance-background').addEventListener('click', openBackgrounds);
$('background-reset').addEventListener('click', async () => { try { await saveSettings({ background: 'default' }, '已恢复默认背景'); await loadBackgrounds(); } catch (error) { toast(error.message, true); } });
$('background-file').addEventListener('change', async () => {
  const file = $('background-file').files[0]; if (!file) return;
  if (file.size > 10 * 1024 * 1024) { toast('图片不能超过 10 MB', true); $('background-file').value = ''; return; }
  $('background-file').disabled = true; $('upload-progress').hidden = false;
  try { const form = new FormData(); form.append('file', file); const background = await api('/api/backgrounds', { method: 'POST', body: form }); await saveSettings({ background: background.id }, '背景已上传并应用'); await loadBackgrounds(); }
  catch (error) { toast(error.message, true); }
  finally { $('background-file').disabled = false; $('background-file').value = ''; $('upload-progress').hidden = true; }
});
$('upload-form').addEventListener('submit', event => event.preventDefault());
$('all-logs').addEventListener('click', () => { hidePassword(); $('logs-dialog').showModal(); loadLogs(1); });
$('logs-prev').addEventListener('click', () => loadLogs(state.logPage - 1));
$('logs-next').addEventListener('click', () => loadLogs(state.logPage + 1));
document.querySelectorAll('.close-dialog').forEach(button => button.addEventListener('click', () => button.closest('dialog').close()));
document.querySelectorAll('dialog').forEach(dialog => dialog.addEventListener('click', event => { if (event.target === dialog) { const r = dialog.getBoundingClientRect(); if (event.clientX < r.left || event.clientX > r.right || event.clientY < r.top || event.clientY > r.bottom) dialog.close(); } }));
systemTheme.addEventListener('change', () => { if (!dirty.appearance) applyAppearance(state.settings); });
$('today').textContent = new Intl.DateTimeFormat('zh-CN', { timeZone: 'Asia/Shanghai', month: 'long', day: 'numeric', weekday: 'long' }).format(new Date());
navigate(location.hash.slice(1));
setInterval(renderDuration, 1000);

async function initialize() {
  try {
    const [settings, account, status, logs] = await Promise.all(['/api/settings', '/api/account', '/api/status', '/api/logs'].map(path => api(path)));
    acceptSnapshot({ settings, account, status, logs }); fillForms(); applyAppearance(settings); serviceConnection(true);
  } catch (error) { toast(error.message, true); }
  const events = new EventSource('/api/events');
  events.onopen = () => serviceConnection(true);
  events.onerror = () => serviceConnection(false);
  events.addEventListener('update', event => { try { acceptSnapshot(JSON.parse(event.data)); } catch { toast('服务返回的数据格式异常，请刷新页面', true); } });
}
initialize();
