// 框选翻译小窗口。框选时 Rust 已经把窗口铺满鼠标所在的显示器，所以窗口坐标 = 显示器坐标；
// 框完先把窗口藏起来再截图，结果出来后窗口缩成卡片那么大，摆在框的旁边。
'use strict';

const TAURI = window.__TAURI__ || null;
const invoke = (cmd, args) => (TAURI ? TAURI.core.invoke(cmd, args) : Promise.resolve(null)).catch((e) => { console.warn(cmd, e); return null; });
const listen = (ev, fn) => (TAURI ? TAURI.event.listen(ev, (e) => fn(e.payload)) : Promise.resolve());

const $ = (s) => document.querySelector(s);
const selectEl = $('#select'), cardEl = $('#card'), boxEl = $('#select .box'), hintEl = $('#select .hint');
const M = 10; // 卡片四周给阴影留的边
let sel = null; // 正在框选：{ active, sx, sy, rect }
let card = null; // 当前卡片的数据
let mon = { w: 1920, h: 1080 };

const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const dpr = () => window.devicePixelRatio || 1;
const snap = (v) => Math.round(v * dpr()) / dpr();
const escapeHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

function toast(msg) {
  const t = $('#toast');
  t.textContent = msg;
  t.classList.add('show');
  clearTimeout(toast._t);
  toast._t = setTimeout(() => t.classList.remove('show'), 1400);
}

// ───────── 框选 ─────────

function startSelect(p) {
  if (p && p.mon) mon = p.mon;
  card = null;
  cardEl.classList.remove('show');
  selectEl.classList.add('show');
  selectEl.classList.remove('dragging');
  boxEl.style.display = 'none';
  hintEl.style.display = 'block';
  sel = { active: false };
}

function endSelect(cancel) {
  selectEl.classList.remove('show', 'dragging');
  const s = sel;
  sel = null;
  if (cancel || !s || !s.rect) {
    invoke('translate_cancel');
    return;
  }
  // 先藏窗口再截图（窗口本来也不会被截进去，这里多等一下保险）
  invoke('win_place', { label: 'sel', x: 0, y: 0, w: 0, h: 0, show: false });
  setTimeout(() => invoke('translate_region', s.rect), 60);
}

selectEl.addEventListener('pointerdown', (e) => {
  if (!sel) return;
  if (e.button === 2) { endSelect(true); return; }
  selectEl.setPointerCapture(e.pointerId);
  sel = { active: true, sx: e.clientX, sy: e.clientY, rect: null };
  selectEl.classList.add('dragging');
  hintEl.style.display = 'none';
});
selectEl.addEventListener('pointermove', (e) => {
  if (!sel || !sel.active) return;
  const x = Math.min(sel.sx, e.clientX), y = Math.min(sel.sy, e.clientY);
  const w = Math.abs(e.clientX - sel.sx), h = Math.abs(e.clientY - sel.sy);
  sel.rect = { x, y, w, h };
  boxEl.style.display = 'block';
  boxEl.style.left = x + 'px'; boxEl.style.top = y + 'px'; boxEl.style.width = w + 'px'; boxEl.style.height = h + 'px';
});
selectEl.addEventListener('pointerup', () => {
  if (!sel || !sel.active) return;
  if (!sel.rect || sel.rect.w < 6 || sel.rect.h < 6) { endSelect(true); return; }
  endSelect(false);
});
document.addEventListener('contextmenu', (e) => { e.preventDefault(); if (sel) endSelect(true); });

// ───────── 翻译卡片 ─────────

/** 卡片摆在框的下面；放不下放上面；再不行放右边/左边（显示器坐标） */
function cardSpot(rect, cw, ch) {
  let x = rect.x, y = rect.y + rect.h + 10;
  if (y + ch > mon.h - 8) y = rect.y - ch - 10;
  if (y < 8) {
    y = clamp(rect.y, 8, mon.h - ch - 8);
    x = rect.x + rect.w + 10;
    if (x + cw > mon.w - 8) x = rect.x - cw - 10;
  }
  return { x: clamp(x, 8, mon.w - cw - 8), y: clamp(y, 8, mon.h - ch - 8) };
}

function showCard(p) {
  if (p.mon) mon = p.mon;
  card = p;
  const body = cardEl.querySelector('.body');
  if (p.loading) body.innerHTML = '<div class="loading"><div class="spin"></div>正在识别和翻译…</div>';
  else if (p.error) body.innerHTML = `<div class="err">${escapeHtml(p.error)}</div>`;
  else body.innerHTML = (p.items || []).map((it) => `<div class="pair"><div class="src">${escapeHtml(it.src || '')}</div><div class="dst">${escapeHtml(it.dst || '')}</div></div>`).join('');
  selectEl.classList.remove('show');
  cardEl.classList.add('show');
  body.scrollTop = 0;
  const cw = cardEl.offsetWidth, ch = cardEl.offsetHeight;
  const at = cardSpot(p.rect, cw, ch);
  invoke('win_place', { label: 'sel', x: snap(at.x - M), y: snap(at.y - M), w: Math.ceil(cw + 2 * M), h: Math.ceil(ch + 2 * M), show: true });
}

function closeCard() {
  card = null;
  cardEl.classList.remove('show');
  invoke('translate_cancel');
}

cardEl.addEventListener('click', (e) => {
  const b = e.target.closest('button');
  if (!b) return;
  const items = (card && card.items) || [];
  const dst = items.map((i) => i.dst).filter(Boolean).join('\n');
  if (b.dataset.act === 'close') closeCard();
  else if (b.dataset.act === 'copy') { invoke('copy_text', { text: dst }); toast('译文已复制'); }
  else if (b.dataset.act === 'read') invoke('test_voice', { text: dst });
});

listen('sel-start', (p) => startSelect(p));
listen('sel-result', (p) => showCard(p));
