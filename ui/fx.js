// 指示圈小窗口：按钮上的圈（点击 / 按下 / 指着）和框选翻译的虚线框。
// 窗口只包住现有的圈，没有圈就藏起来；永远鼠标穿透（在 Rust 里建窗口时设好，不来回切）。
'use strict';

const TAURI = window.__TAURI__ || null;
const invoke = (cmd, args) => (TAURI ? TAURI.core.invoke(cmd, args) : Promise.resolve(null)).catch((e) => { console.warn(cmd, e); return null; });
const listen = (ev, fn) => (TAURI ? TAURI.event.listen(ev, (e) => fn(e.payload)) : Promise.resolve());

const layer = document.getElementById('layer');
const rings = []; // { x, y, w, h, kind, el, timer }
let box = null; // 窗口在显示器坐标里的位置
let hideTimer = 0;

const dpr = () => window.devicePixelRatio || 1;
const down = (v) => Math.floor(v * dpr()) / dpr();
const up = (v) => Math.ceil(v * dpr()) / dpr();
// 圈会放大、发光、出涟漪，四周留出余量
const pad = (r) => Math.round(Math.max(28, Math.max(r.w, r.h) * 0.2 + 22));

function layout() {
  if (!rings.length) {
    // 最后一个圈的动画放完再藏
    clearTimeout(hideTimer);
    hideTimer = setTimeout(() => {
      if (rings.length || !box) return;
      box = null;
      layer.innerHTML = '';
      invoke('win_place', { label: 'fx', x: 0, y: 0, w: 0, h: 0, show: false });
    }, 120);
    return;
  }
  clearTimeout(hideTimer);
  let l = Infinity, t = Infinity, r = -Infinity, b = -Infinity;
  for (const g of rings) {
    const p = pad(g);
    l = Math.min(l, g.x - p); t = Math.min(t, g.y - p);
    r = Math.max(r, g.x + g.w + p); b = Math.max(b, g.y + g.h + p);
  }
  const nb = { x: down(l), y: down(t), w: up(r) - down(l), h: up(b) - down(t) };
  const moved = !box || nb.x !== box.x || nb.y !== box.y || nb.w !== box.w || nb.h !== box.h;
  box = nb;
  for (const g of rings) {
    g.el.style.left = g.x - box.x - 4 + 'px';
    g.el.style.top = g.y - box.y - 4 + 'px';
    if (g.ripple) { g.ripple.style.left = g.x - box.x + g.w / 2 + 'px'; g.ripple.style.top = g.y - box.y + g.h / 2 + 'px'; }
  }
  if (moved) invoke('win_place', { label: 'fx', x: box.x, y: box.y, w: box.w, h: box.h, show: true });
}

function remove(g) {
  const i = rings.indexOf(g);
  if (i < 0) return;
  rings.splice(i, 1);
  clearTimeout(g.timer);
  g.el.remove();
  if (g.ripple) g.ripple.remove();
  layout();
}

/** kind: click 点击（圈 + 涟漪）/ press 按下帧（立刻出涟漪）/ point 指着（呼吸光圈）/ region 翻译的虚线框；ms=0 一直留着 */
function add(p) {
  const kind = p.kind === 'press' ? 'click' : p.kind;
  const el = document.createElement('div');
  el.className = 'hl ' + kind;
  el.style.width = p.w + 8 + 'px';
  el.style.height = p.h + 8 + 'px';
  const g = { x: p.x, y: p.y, w: p.w, h: p.h, kind: p.kind, el };
  if (kind === 'click') {
    const r = document.createElement('div');
    r.className = 'ripple';
    g.ripple = r;
    if (p.kind === 'press') layer.appendChild(r);
    else setTimeout(() => { if (rings.includes(g)) layer.appendChild(r); }, 160);
  }
  layer.appendChild(el);
  rings.push(g);
  layout();
  if (p.ms !== 0) g.timer = setTimeout(() => remove(g), p.ms || 1200);
}

listen('fx-highlight', (p) => add(p));
listen('fx-clear', (p) => {
  for (const g of rings.slice()) if (!p || !p.kind || g.kind === p.kind) remove(g);
});
