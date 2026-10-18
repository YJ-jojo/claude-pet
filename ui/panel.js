// 对话框：聊天、选要看的窗口、设置。宠物在舞台层里，这里只是控制台。
'use strict';

const TAURI = window.__TAURI__ || null;
const call = (cmd, args) => (TAURI ? TAURI.core.invoke(cmd, args) : Promise.resolve(null));
const invoke = (cmd, args) => call(cmd, args).catch((e) => { console.warn(cmd, e); return null; });
const listen = (ev, fn) => (TAURI ? TAURI.event.listen(ev, (e) => fn(e.payload)) : Promise.resolve());

const $ = (s, r = document) => r.querySelector(s);
const $$ = (s, r = document) => Array.from(r.querySelectorAll(s));

const P = {
  cfg: {},
  flags: {},
  tab: 'chat',
  mode: 'chat',
  msgs: new Map(),
  wins: [],
  edgeVoices: [],
  packs: [],
};

function toast(msg, ms = 1800) {
  const t = $('#toast');
  t.textContent = msg;
  t.classList.add('show');
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.remove('show'), ms);
}

const esc = (s) => String(s ?? '').replace(/[&<>"]/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]));

// ───────────────────────── 标签页 ─────────────────────────

function showTab(tab) {
  if (tab === 'operate' || tab === 'point') {
    setMode(tab);
    tab = 'chat';
  }
  if (!['chat', 'watch', 'settings'].includes(tab)) tab = 'chat';
  P.tab = tab;
  $$('nav button').forEach((b) => b.classList.toggle('on', b.dataset.tab === tab));
  $$('.tab').forEach((s) => s.classList.toggle('on', s.id === 'tab-' + tab));
  if (tab === 'chat') setTimeout(() => $('#txt').focus(), 30);
  if (tab === 'watch') refreshWindows();
  if (tab === 'settings') refreshStatus();
}
$$('nav button').forEach((b) => b.addEventListener('click', () => showTab(b.dataset.tab)));

// ───────────────────────── 对话 ─────────────────────────

const KIND_TAG = { alert: '🔔 提醒', breath: '🫧 呼吸模式', look: '👁 看了一眼', error: '⚠ 出错', op: '🖱 操作' };
const msgsEl = $('#msgs');

function nearBottom() {
  return msgsEl.scrollHeight - msgsEl.scrollTop - msgsEl.clientHeight < 80;
}
function toBottom() {
  msgsEl.scrollTop = msgsEl.scrollHeight;
}

function addMsg(p) {
  $('#msgs-empty')?.remove();
  const stick = nearBottom();
  const el = document.createElement('div');
  const role = p.role || 'pet';
  el.className = `msg ${role}` + (p.kind && p.kind !== 'talk' ? ` k-${p.kind}` : '');
  const tag = role === 'pet' && KIND_TAG[p.kind] ? `<span class="tag">${KIND_TAG[p.kind]}</span>` : '';
  el.innerHTML = `${tag}<span class="body"></span>`;
  el.querySelector('.body').textContent = p.text || '';
  // 宠物先发一个空泡再流式追加文字
  if (role === 'pet' && !p.text) el.classList.add('streaming');
  msgsEl.appendChild(el);
  if (p.id) P.msgs.set(p.id, el);
  // 只留最近 300 条
  while (msgsEl.children.length > 300) {
    const first = msgsEl.firstElementChild;
    for (const [k, v] of P.msgs) if (v === first) P.msgs.delete(k);
    first.remove();
  }
  if (stick || role === 'user') toBottom();
}

function onChat(p) {
  if (!p) return;
  if (p.mode === 'new') return addMsg(p);
  const el = P.msgs.get(p.id);
  if (!el) {
    if (p.mode === 'append') addMsg({ id: p.id, role: 'pet', text: p.text });
    return;
  }
  const stick = nearBottom();
  if (p.mode === 'append') el.querySelector('.body').textContent += p.text;
  else if (p.mode === 'done') {
    el.classList.remove('streaming');
    if (!el.querySelector('.body').textContent.trim()) el.remove();
  }
  if (stick) toBottom();
}

function setMode(m) {
  P.mode = m;
  $$('#modes [data-mode]').forEach((b) => b.classList.toggle('on', b.dataset.mode === m));
  $('#txt').placeholder = {
    chat: '跟我说点什么…（Enter 发送，Shift+Enter 换行）',
    operate: '要我帮你做什么？比如“点保存”“在搜索框里输入天气”',
    point: '要找哪个按钮？我跑过去指给你看',
  }[m];
}
$$('#modes [data-mode]').forEach((b) => b.addEventListener('click', () => { setMode(b.dataset.mode); $('#txt').focus(); }));

function send() {
  const ta = $('#txt');
  const text = ta.value.trim();
  if (!text) return;
  ta.value = '';
  autoGrow();
  invoke('send_chat', { text, mode: P.mode });
}
function autoGrow() {
  const ta = $('#txt');
  ta.style.height = 'auto';
  ta.style.height = Math.min(140, Math.max(40, ta.scrollHeight)) + 'px';
}
$('#txt').addEventListener('input', autoGrow);
$('#txt').addEventListener('keydown', (e) => {
  if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
    e.preventDefault();
    send();
  }
});
$('#btn-send').addEventListener('click', send);
$('#btn-look').addEventListener('click', () => {
  const q = $('#txt').value.trim();
  if (q) { $('#txt').value = ''; autoGrow(); }
  invoke('look', { question: q || null });
});
$('#btn-stop').addEventListener('click', () => invoke('stop_all'));

// ───────────────────────── 状态 ─────────────────────────

const MODE_CN = { quiet: '安静', normal: '正常', detailed: '细讲' };

function renderFlags() {
  const f = P.flags;
  const dot = $('#st-dot');
  let text = '待命', cls = '';
  if (f.operating) { text = '正在帮你操作'; cls = 'op'; }
  else if (f.thinking) { text = '在想…'; cls = 'think'; }
  else if (f.looking) { text = '在看…'; cls = 'look'; }
  else if (f.speaking) { text = '在说话'; cls = f.breathing ? 'breath' : ''; }
  else if (f.breathing) { text = `呼吸模式（${MODE_CN[f.breathMode] || '正常'}）`; cls = 'breath'; }
  if (f.breathing && cls !== 'breath') text += ' · 呼吸模式开着';
  if (f.targets) text += ` · 盯着 ${f.targets} 个窗口`;
  dot.className = 'dot ' + cls;
  $('#st-text').textContent = text;
  $('#breath-on').checked = !!f.breathing;
  const m = f.breathMode || P.cfg.breath_mode || 'normal';
  $$('#breath-mode button').forEach((b) => b.classList.toggle('on', b.dataset.v === m));
}

// ───────────────────────── 观察 ─────────────────────────

$('#breath-on').addEventListener('change', (e) => invoke('toggle_breath', { on: e.target.checked }));
$$('#breath-mode button').forEach((b) => b.addEventListener('click', () => {
  P.flags.breathMode = b.dataset.v;
  renderFlags();
  invoke('menu_action', { action: 'mode-' + b.dataset.v });
}));

async function refreshWindows() {
  const list = await invoke('list_windows');
  if (Array.isArray(list)) P.wins = list;
  renderWindows();
}

function renderWindows() {
  const q = $('#win-filter').value.trim().toLowerCase();
  const box = $('#wins');
  const list = P.wins
    .filter((w) => !q || (w.title + ' ' + w.process).toLowerCase().includes(q))
    .sort((a, b) => (b.watching - a.watching));
  if (!list.length) {
    box.innerHTML = `<div class="win"><span class="t sub">${q ? '没有匹配的窗口' : '没找到窗口，点“刷新”再试试'}</span></div>`;
    return;
  }
  box.innerHTML = list.map((w) => `
    <label class="win${w.watching ? ' on' : ''}${w.blocked ? ' blocked' : ''}" data-h="${w.hwnd}" title="${esc(w.title)}">
      <input type="checkbox" ${w.watching ? 'checked' : ''} ${w.blocked ? 'disabled' : ''} />
      <span class="t">${esc(w.title)}</span>
      ${w.blocked ? '<span class="badge">不看</span>' : w.minimized ? '<span class="badge">最小化</span>' : ''}
      <span class="p">${esc(w.process.replace(/\.exe$/i, ''))}</span>
    </label>`).join('');
  const n = P.wins.filter((w) => w.watching).length;
  $('#watch-sub').textContent = n ? `正在盯着 ${n} 个窗口` : '一个都不勾时，我看你正在用的窗口';
}

$('#wins').addEventListener('change', (e) => {
  const row = e.target.closest('.win');
  if (!row) return;
  const hwnd = Number(row.dataset.h);
  const on = e.target.checked;
  const w = P.wins.find((x) => x.hwnd === hwnd);
  if (w) w.watching = on;
  row.classList.toggle('on', on);
  invoke('set_target', { hwnd, on });
});
$('#win-filter').addEventListener('input', renderWindows);
$('#btn-refresh').addEventListener('click', refreshWindows);
$('#btn-clear').addEventListener('click', async () => { await invoke('clear_targets'); refreshWindows(); });
$('#btn-pick').addEventListener('click', async (e) => {
  const b = e.currentTarget;
  if (b.disabled) return;
  b.disabled = true;
  let left = 3;
  b.textContent = `把鼠标移到要看的窗口上… ${left}`;
  const t = setInterval(() => { left -= 1; if (left > 0) b.textContent = `把鼠标移到要看的窗口上… ${left}`; }, 1000);
  const w = await invoke('pick_window_delayed', { ms: 3000 });
  clearInterval(t);
  b.disabled = false;
  b.textContent = '3 秒后选鼠标下的窗口';
  toast(w ? `已加入：${w.title.slice(0, 24)}` : '鼠标下面没找到窗口');
  refreshWindows();
});
$('#blacklist').addEventListener('change', (e) => {
  const list = e.target.value.split(/[\n,，]/).map((s) => s.trim()).filter(Boolean);
  save({ blacklist: list });
});

// ───────────────────────── 设置 ─────────────────────────

// 左边分类，一次只显示一类；记住上次看的那类
function showCat(cat) {
  if (!$(`.grp[data-cat="${cat}"]`)) cat = 'account';
  $$('#cats button').forEach((b) => b.classList.toggle('on', b.dataset.cat === cat));
  $$('.grp').forEach((g) => g.classList.toggle('on', g.dataset.cat === cat));
  $('#tab-settings .pane').scrollTop = 0;
  try { localStorage.setItem('pet-cat', cat); } catch (_) {}
}
$$('#cats button').forEach((b) => b.addEventListener('click', () => showCat(b.dataset.cat)));
try { showCat(localStorage.getItem('pet-cat') || 'account'); } catch (_) { showCat('account'); }

let saveTimer = null, pending = {};
function save(part, now = false) {
  Object.assign(pending, part);
  Object.assign(P.cfg, part);
  clearTimeout(saveTimer);
  const go = async () => {
    const body = pending;
    pending = {};
    try {
      await call('save_config', { config: body });
      toast('已保存', 900);
    } catch (e) {
      toast('保存失败：' + e, 3000);
    }
  };
  if (now) go(); else saveTimer = setTimeout(go, 250);
}

function readField(el) {
  if (el.type === 'checkbox') return el.checked;
  if ('num' in el.dataset) {
    const v = 'int' in el.dataset ? parseInt(el.value, 10) : parseFloat(el.value);
    return Number.isFinite(v) ? v : null;
  }
  return el.value;
}

function fillFields() {
  const c = P.cfg;
  $('#pet-name').textContent = c.pet_name || '宠物';
  $$('[data-k]').forEach((el) => {
    if (el === document.activeElement && el.type !== 'checkbox') return;
    const v = c[el.dataset.k];
    if (v === undefined || v === null) return;
    if (el.type === 'checkbox') el.checked = !!v;
    else el.value = v;
  });
  if (document.activeElement !== $('#blacklist')) $('#blacklist').value = (c.blacklist || []).join('\n');
  labels();
}

function labels() {
  const c = P.cfg;
  $('#rate-v').textContent = `${Number(c.tts_rate || 1).toFixed(2)}×`;
  $('#h-v').textContent = `${c.pet_height || 240}px`;
  $('#spd-v').textContent = `${c.run_speed || 2600}`;
  const edge = (c.tts_engine || 'edge') === 'edge';
  $('#f-edge').style.display = edge ? '' : 'none';
  $('#f-sys').style.display = edge ? 'none' : '';
}

$$('[data-k]').forEach((el) => {
  if (el.classList.contains('hk')) return;
  const k = el.dataset.k;
  if (el.type === 'range') {
    el.addEventListener('input', () => { P.cfg[k] = readField(el); labels(); });
  }
  el.addEventListener('change', () => {
    const v = readField(el);
    if (v === null) return;
    save({ [k]: v }, el.type === 'checkbox' || el.tagName === 'SELECT');
    labels();
  });
});

function fillVoices(sys) {
  $('#edge-voice').innerHTML = P.edgeVoices.map((v) => `<option value="${esc(v.id)}">${esc(v.name)}</option>`).join('');
  if (Array.isArray(sys)) {
    $('#sys-voice').innerHTML = '<option value="">默认</option>' + sys.map((v) => `<option value="${esc(v)}">${esc(v)}</option>`).join('');
  }
  $('#packs').innerHTML = P.packs.map((p) => `<option value="${esc(p)}">${p === 'placeholder' ? '占位形象（内置）' : esc(p)}</option>`).join('');
  fillFields();
}

$('#btn-test-voice').addEventListener('click', () => invoke('test_voice', { text: null }));
$('#btn-sprites').addEventListener('click', () => invoke('open_sprites_dir'));
$('#btn-log').addEventListener('click', () => invoke('open_log_dir'));
$('#btn-quit').addEventListener('click', () => invoke('menu_action', { action: 'quit' }));
$('#btn-update').addEventListener('click', () => { invoke('menu_action', { action: 'update' }); toast('在检查，结果宠物会告诉你'); });

// 快捷键录制：点进输入框后直接按组合键
const CODE_NAME = { Space: 'Space', Enter: 'Enter', Tab: 'Tab', Backquote: '`', Minus: '-', Equal: '=', BracketLeft: '[', BracketRight: ']', Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',', Period: '.', Slash: '/' };
$$('.hk').forEach((el) => {
  el.readOnly = true;
  el.addEventListener('focus', () => { el.classList.add('rec'); el.dataset.old = el.value; el.value = ''; el.placeholder = '按下组合键…（Esc 取消，Backspace 清空）'; });
  el.addEventListener('blur', () => { el.classList.remove('rec'); if (!el.value && el.dataset.old !== undefined && !el.dataset.cleared) el.value = el.dataset.old; delete el.dataset.cleared; el.placeholder = ''; });
  el.addEventListener('keydown', (e) => {
    e.preventDefault();
    if (e.key === 'Escape') { el.value = el.dataset.old || ''; el.blur(); return; }
    if ((e.key === 'Backspace' || e.key === 'Delete') && !e.ctrlKey && !e.altKey && !e.shiftKey) {
      el.value = '';
      el.dataset.cleared = '1';
      save({ [el.dataset.k]: '' }, true);
      el.blur();
      return;
    }
    if (['Control', 'Alt', 'Shift', 'Meta'].includes(e.key)) return;
    let key = null;
    if (/^Key[A-Z]$/.test(e.code)) key = e.code.slice(3);
    else if (/^Digit\d$/.test(e.code)) key = e.code.slice(5);
    else if (/^F\d{1,2}$/.test(e.code)) key = e.code;
    else if (CODE_NAME[e.code]) key = CODE_NAME[e.code];
    else if (/^Arrow/.test(e.code)) key = e.code;
    if (!key) return;
    const mods = [];
    if (e.ctrlKey) mods.push('CommandOrControl');
    if (e.altKey) mods.push('Alt');
    if (e.shiftKey) mods.push('Shift');
    if (e.metaKey) mods.push('Super');
    if (!mods.length && !/^F\d/.test(key)) { toast('要配合 Ctrl / Alt / Shift 一起按'); return; }
    const combo = [...mods, key].join('+');
    const dup = $$('.hk').find((o) => o !== el && o.value === combo);
    if (dup) { toast('这个组合已经给别的功能用了'); return; }
    el.value = combo;
    save({ [el.dataset.k]: combo }, true);
    el.blur();
  });
});
const prettyHotkey = (s) => s.replace(/CommandOrControl/g, 'Ctrl');
$$('.hk').forEach((el) => { el.title = '点一下再按组合键'; });

// Claude Code 状态
async function refreshStatus() {
  const box = $('#cc-status'), dbox = $('#desk-status');
  const s = await invoke('claude_status');
  if (!s) { box.innerHTML = '<div class="sub bad">检查失败</div>'; return; }
  const row = (k, v, cls = '') => `<span class="k">${k}</span><span class="v ${cls}">${v}</span>`;
  const authLine = (s.auth || '').split('\n').map((l) => l.trim()).filter(Boolean).slice(0, 2).join('；');
  box.innerHTML = s.found
    ? row('版本', `✅ ${esc(s.version || '已安装')}`) +
      row('位置', `<span class="sub">${esc(s.path)}</span>`) +
      row('登录', s.loggedIn ? `✅ ${esc(authLine || '已登录')}` : '⚠ 还没登录（点下面“登录”）', s.loggedIn ? '' : 'bad') +
      row('文字识别', s.ocrLangs && s.ocrLangs.length ? esc(s.ocrLangs.join('、')) : '没有 OCR 语言包（不影响大部分窗口）')
    : row('状态', '❌ 还没装（点下面“一键安装”）', 'bad');
  $('#btn-install').hidden = !!s.found;
  $('#btn-login').hidden = !s.found || !!s.loggedIn;
  const d = s.desktop || {};
  dbox.innerHTML =
    row('桌面版', d.installed ? (d.registered ? '✅ 已接入（重开桌面版后生效）' : '⚠ 还没接入') : '没找到 Claude 桌面版的设置文件夹', d.registered ? '' : 'bad') +
    row('确认提醒', s.hooks ? '✅ 已安装' : '还没装', s.hooks ? '' : 'bad');
  $('#btn-desk').textContent = d.registered ? '重新接入' : '接入';
  $('#btn-hooks').textContent = s.hooks ? '重新安装' : '安装';
}

async function runBtn(btn, cmd, busy, msgSel = '#cc-msg') {
  const msg = $(msgSel);
  const old = btn.textContent;
  btn.disabled = true;
  btn.textContent = busy;
  try {
    const r = await call(cmd);
    msg.className = 'sub ok';
    msg.textContent = r || '好了';
  } catch (e) {
    msg.className = 'sub bad';
    msg.textContent = String(e);
  }
  btn.disabled = false;
  btn.textContent = old;
  refreshStatus();
}
$('#btn-install').addEventListener('click', (e) => { $('#setup-details').open = true; runBtn(e.currentTarget, 'cli_install', '正在安装…（不用管它）'); });
$('#btn-login').addEventListener('click', async (e) => {
  $('#login-box').hidden = false;
  await runBtn(e.currentTarget, 'cli_login', '等你在浏览器里授权…');
  $('#login-box').hidden = true;
});
$('#btn-code').addEventListener('click', async () => {
  const v = $('#login-code').value.trim();
  if (!v) return;
  try { await call('cli_login_code', { code: v }); toast('已提交授权码'); $('#login-code').value = ''; } catch (e) { toast(String(e)); }
});
$('#btn-desk').addEventListener('click', (e) => runBtn(e.currentTarget, 'desktop_register', '接入中…', '#desk-msg'));
$('#btn-undesk').addEventListener('click', (e) => runBtn(e.currentTarget, 'desktop_unregister', '移除中…', '#desk-msg'));
$('#btn-hooks').addEventListener('click', (e) => runBtn(e.currentTarget, 'install_hooks', '安装中…', '#desk-msg'));
$('#btn-unhooks').addEventListener('click', (e) => runBtn(e.currentTarget, 'uninstall_hooks', '移除中…', '#desk-msg'));
$('#btn-mcp').addEventListener('click', (e) => runBtn(e.currentTarget, 'install_mcp', '注册中…', '#desk-msg'));
$('#btn-recheck').addEventListener('click', refreshStatus);
listen('setup-log', (p) => {
  const pre = $('#setup-log');
  pre.textContent += (p && p.line ? p.line : '') + '\n';
  pre.scrollTop = pre.scrollHeight;
});
listen('login-url', (p) => {
  const a = $('#login-url');
  a.href = p.url;
  a.onclick = (ev) => { ev.preventDefault(); call('open_url', { url: p.url }).catch(() => {}); };
  $('#login-box').hidden = false;
});

// ───────────────────────── 启动 ─────────────────────────

async function init() {
  setMode('chat');
  const st = await invoke('get_state');
  if (st) {
    P.cfg = st.config || {};
    P.edgeVoices = st.edgeVoices || [];
    P.packs = st.packs || ['placeholder'];
    P.flags = { breathing: st.breathing, breathMode: P.cfg.breath_mode, targets: (st.targets || []).filter((t) => t.enabled).length };
    $('#ver').textContent = `Claude 宠物 ${st.version}`;
    $('#ver2').textContent = `现在是 ${st.version}`;
  }
  fillVoices(null);
  renderFlags();
  const sys = await invoke('system_voices');
  fillVoices(sys);
}

listen('chat', onChat);
listen('open-tab', (p) => showTab((p && p.tab) || 'chat'));
listen('flags', (f) => { P.flags = Object.assign({}, P.flags, f); renderFlags(); });
listen('config', (c) => { P.cfg = Object.assign({}, P.cfg, c); fillFields(); renderFlags(); });
listen('targets', () => { if (P.tab === 'watch') refreshWindows(); });

document.addEventListener('keydown', (e) => {
  if (e.ctrlKey && !e.altKey && ['1', '2', '3'].includes(e.key)) {
    e.preventDefault();
    showTab(['chat', 'watch', 'settings'][Number(e.key) - 1]);
  }
});

// 窗口刚建好时要打开的标签页（commands.rs 的 open_panel_tab 写进 window.__PET_TAB__）
if (window.__PET_TAB__) showTab(window.__PET_TAB__);
init();
