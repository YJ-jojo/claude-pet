// 指示圈和框选翻译。以前各是一个小窗口（fx.html、sel.html），可在用户电脑上每建一个
// WebView2 窗口要 8–20 秒、建的时候整个程序卡住，所以 0.3.1 起都画在宠物窗口的 world 里：
// world 本来就是显示器坐标，用到时让宠物窗口临时变大包住它们（stage.js 的 needBox 会问 overlayBoxes）。
// 依赖 stage.js 里的 S、$、invoke、listen、toast、worldRect、sendRects、now、clamp、snap。
'use strict';

const fxEl = $('#fx'), selEl = $('#select'), selBoxEl = $('#select .box'), selHintEl = $('#select .hint'), cardEl = $('#card');
const OV = {
  rings: [], // { x, y, w, h, kind, el, ripple, timer }
  sel: null, // 正在框选：{ active, sx, sy, rect }
  card: null, // 翻译卡片的数据
  cardAt: null, // 卡片在 world 里的位置
};

// 圈会放大、发光、出涟漪，四周留出余量
const ringPad = (r) => Math.round(Math.max(28, Math.max(r.w, r.h) * 0.2 + 22));

/** 宠物窗口要额外包住的范围（world 坐标） */
function overlayBoxes() {
  const out = [];
  for (const g of OV.rings) {
    const p = ringPad(g);
    out.push({ x: g.x - p, y: g.y - p, w: g.w + 2 * p, h: g.h + 2 * p });
  }
  if (OV.sel) out.push({ x: 0, y: 0, w: S.mon.w, h: S.mon.h });
  if (OV.card && OV.cardAt) out.push({ x: OV.cardAt.x - 10, y: OV.cardAt.y - 10, w: cardEl.offsetWidth + 20, h: cardEl.offsetHeight + 20 });
  return out;
}

/** 宠物窗口里要接鼠标的地方（圈永远不接） */
function overlayHitRects() {
  if (OV.sel) return [{ x: 0, y: 0, w: S.mon.w, h: S.mon.h }];
  if (OV.card && OV.cardAt) return [{ x: OV.cardAt.x - 2, y: OV.cardAt.y - 2, w: cardEl.offsetWidth + 4, h: cardEl.offsetHeight + 4 }];
  return [];
}

// ───────── 指示圈 ─────────

function removeRing(g) {
  const i = OV.rings.indexOf(g);
  if (i < 0) return;
  OV.rings.splice(i, 1);
  clearTimeout(g.timer);
  g.el.remove();
  if (g.ripple) g.ripple.remove();
}

/** kind: click 点击（圈 + 涟漪）/ press 按下帧（立刻出涟漪）/ point 指着（呼吸光圈）/ region 翻译的虚线框；ms=0 一直留着 */
function addRing(p) {
  const kind = p.kind === 'press' ? 'click' : p.kind;
  const el = document.createElement('div');
  el.className = 'hl ' + kind;
  el.style.left = p.x - 4 + 'px';
  el.style.top = p.y - 4 + 'px';
  el.style.width = p.w + 8 + 'px';
  el.style.height = p.h + 8 + 'px';
  const g = { x: p.x, y: p.y, w: p.w, h: p.h, kind: p.kind, el };
  if (kind === 'click') {
    const r = document.createElement('div');
    r.className = 'ripple';
    r.style.left = p.x + p.w / 2 + 'px';
    r.style.top = p.y + p.h / 2 + 'px';
    g.ripple = r;
    if (p.kind === 'press') fxEl.appendChild(r);
    else setTimeout(() => { if (OV.rings.includes(g)) fxEl.appendChild(r); }, 160);
  }
  fxEl.appendChild(el);
  OV.rings.push(g);
  if (p.ms !== 0) g.timer = setTimeout(() => removeRing(g), p.ms || 1200);
}

listen('fx-highlight', (p) => addRing(p));
listen('fx-clear', (p) => {
  for (const g of OV.rings.slice()) if (!p || !p.kind || g.kind === p.kind) removeRing(g);
});

// ───────── 框选 ─────────

function startSelect(p) {
  if (p && p.mon) { S.mon = p.mon; sizeWorld(); }
  closeCard(true);
  selEl.style.width = S.mon.w + 'px';
  selEl.style.height = S.mon.h + 'px';
  selEl.classList.add('show');
  selEl.classList.remove('dragging');
  selBoxEl.style.display = 'none';
  selHintEl.style.display = 'block';
  OV.sel = { active: false };
  invoke('set_capture_all', { on: true });
  sendRects(now(), true);
}

function endSelect(cancel) {
  selEl.classList.remove('show', 'dragging');
  const s = OV.sel;
  OV.sel = null;
  invoke('set_capture_all', { on: false });
  sendRects(now(), true);
  if (cancel || !s || !s.rect) {
    invoke('translate_cancel');
    return;
  }
  // 蒙版先撤掉、画面刷新两帧后再截图
  requestAnimationFrame(() => requestAnimationFrame(() => setTimeout(() => invoke('translate_region', s.rect), 30)));
}

selEl.addEventListener('pointerdown', (e) => {
  e.stopPropagation();
  if (!OV.sel) return;
  if (e.button === 2) { endSelect(true); return; }
  selEl.setPointerCapture(e.pointerId);
  OV.sel = { active: true, sx: wx(e), sy: wy(e), rect: null };
  selEl.classList.add('dragging');
  selHintEl.style.display = 'none';
});
selEl.addEventListener('pointermove', (e) => {
  const s = OV.sel;
  if (!s || !s.active) return;
  const x = Math.min(s.sx, wx(e)), y = Math.min(s.sy, wy(e));
  const w = Math.abs(wx(e) - s.sx), h = Math.abs(wy(e) - s.sy);
  s.rect = { x, y, w, h };
  selBoxEl.style.display = 'block';
  selBoxEl.style.left = x + 'px'; selBoxEl.style.top = y + 'px'; selBoxEl.style.width = w + 'px'; selBoxEl.style.height = h + 'px';
});
selEl.addEventListener('pointerup', () => {
  const s = OV.sel;
  if (!s || !s.active) return;
  if (!s.rect || s.rect.w < 6 || s.rect.h < 6) { endSelect(true); return; }
  endSelect(false);
});
selEl.addEventListener('contextmenu', (e) => { e.preventDefault(); e.stopPropagation(); if (OV.sel) endSelect(true); });

listen('sel-start', (p) => startSelect(p));
listen('sel-clear', () => {
  if (OV.sel) { selEl.classList.remove('show', 'dragging'); OV.sel = null; invoke('set_capture_all', { on: false }); }
  closeCard(true);
});

// ───────── 翻译卡片 ─────────

const escHtml = (s) => String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));

/** 卡片摆在框的下面；放不下放上面；再不行放右边/左边（world 坐标） */
function cardSpot(rect, cw, ch) {
  const mon = S.mon;
  let x = rect.x, y = rect.y + rect.h + 10;
  if (y + ch > mon.h - 8) y = rect.y - ch - 10;
  if (y < 8) {
    y = clamp(rect.y, 8, mon.h - ch - 8);
    x = rect.x + rect.w + 10;
    if (x + cw > mon.w - 8) x = rect.x - cw - 10;
  }
  return { x: snap(clamp(x, 8, mon.w - cw - 8)), y: snap(clamp(y, 8, mon.h - ch - 8)) };
}

function showCard(p) {
  if (p.mon) { S.mon = p.mon; sizeWorld(); }
  OV.card = p;
  const body = cardEl.querySelector('.body');
  if (p.loading) body.innerHTML = '<div class="loading"><div class="spin"></div>正在识别和翻译…</div>';
  else if (p.error) body.innerHTML = `<div class="err">${escHtml(p.error)}</div>`;
  else body.innerHTML = (p.items || []).map((it) => `<div class="pair"><div class="src">${escHtml(it.src || '')}</div><div class="dst">${escHtml(it.dst || '')}</div></div>`).join('');
  cardEl.classList.add('show');
  body.scrollTop = 0;
  OV.cardAt = cardSpot(p.rect, cardEl.offsetWidth, cardEl.offsetHeight);
  cardEl.style.left = OV.cardAt.x + 'px';
  cardEl.style.top = OV.cardAt.y + 'px';
  sendRects(now(), true);
}

/** quiet：Rust 那边已经知道了（新的框选、取消），不用再告诉它 */
function closeCard(quiet) {
  if (!OV.card) return;
  OV.card = null;
  OV.cardAt = null;
  cardEl.classList.remove('show');
  sendRects(now(), true);
  if (!quiet) invoke('translate_cancel');
}

cardEl.addEventListener('pointerdown', (e) => e.stopPropagation());
cardEl.addEventListener('click', (e) => {
  const b = e.target.closest('button');
  if (!b) return;
  const items = (OV.card && OV.card.items) || [];
  const dst = items.map((i) => i.dst).filter(Boolean).join('\n');
  if (b.dataset.act === 'close') closeCard(false);
  else if (b.dataset.act === 'copy') { invoke('copy_text', { text: dst }); toast('译文已复制'); }
  else if (b.dataset.act === 'read') invoke('test_voice', { text: dst });
});

listen('sel-result', (p) => showCard(p));
