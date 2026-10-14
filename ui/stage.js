// 宠物窗口：宠物、气泡、右键菜单。
// 排版用“显示器坐标”（#world，CSS 像素，显示器左上角为原点），窗口本身只是 world 上跟着宠物走的一小块：
// 宠物跑的时候挪的是整个窗口，窗口里的画面不动，所以不会有“画面和窗口差一帧”的抖动。
// （0.1 版是一个铺满屏幕的透明窗口，系统每帧都要合成整屏透明层，卡。）
// 指示圈在 fx.html，框选翻译在 sel.html，各是一个用到时才出现的小窗口。
// 动作怎么播在 anim.js；这里决定“什么时候播哪个动作”，对应《动作设计表》第 2 节的组合动作。
'use strict';

const TAURI = window.__TAURI__ || null;
const invoke = (cmd, args) => (TAURI ? TAURI.core.invoke(cmd, args) : Promise.resolve(null)).catch((e) => { console.warn(cmd, e); return null; });
const listen = (ev, fn) => (TAURI ? TAURI.event.listen(ev, (e) => fn(e.payload)) : Promise.resolve());

const $ = (s) => document.querySelector(s);
const petEl = $('#pet'), flipEl = $('#pet-flip'), bodyEl = $('#pet-body'), imgEl = $('#pet-img'), canvas = $('#pet-canvas');
const bubbleEl = $('#bubble'), menuEl = $('#menu'), worldEl = $('#world'), toastEl = $('#toast');
const anim = new PetAnim({ body: bodyEl, flip: flipEl, img: imgEl, canvas, prev: $('#pet-prev') });

const S = {
  cfg: { pet_height: 240, wander: false, tts_enabled: true, breath_mode: 'normal' },
  work: { x: 0, y: 0, w: 1920, h: 1040 },
  mon: { w: 1920, h: 1080 },
  pet: { x: 1700, y: 1040, w: 260, h: 240, face: 1 },
  // 窗口：rel 是窗口四边相对脚底点的偏移，V 是窗口在 world 里的位置
  win: { rel: null, V: { x: 0, y: 0, w: 0, h: 0 }, sent: '', shown: false, shrinkAt: 0 },
  aspect: 1.09, anchor: [0.5, 1.0], eye: null, packBase: '', decodeH: 300,
  body: [0, 0, 1, 1], // 角色在画面里实际占的范围（素材四周可能留白）
  flags: {}, speaking: false,
  walk: null, drag: null,
  bubble: { id: null, done: true, hideAt: 0, hover: false, kind: 'talk' },
  menuOpen: false, menuLeaveAt: 0, toastUntil: 0,
  lastActive: performance.now(),
  nextWander: performance.now() + 20000,
  nextIdle: performance.now() + 9000,
  sleep: null, // null / 'dozing' / 'sleep'
  alertUntil: 0,
  pointing: null, typing: false, op: null,
  petting: false, headSince: 0, pokes: [],
  lastRects: '', lastRectSend: 0, lastReport: 0,
};

const clamp = (v, a, b) => Math.max(a, Math.min(b, v));
const now = () => performance.now();
const sleepMs = (ms) => new Promise((r) => setTimeout(r, ms));
const dpr = () => window.devicePixelRatio || 1;
/** 对齐到物理像素，窗口和画面都落在整像素上，不会糊、不会抖 */
const snap = (v) => Math.round(v * dpr()) / dpr();
const snapDown = (v) => Math.floor(v * dpr()) / dpr();
const snapUp = (v) => Math.ceil(v * dpr()) / dpr();
/** 鼠标事件的窗口坐标 → world 坐标 */
const wx = (e) => e.clientX + S.win.V.x;
const wy = (e) => e.clientY + S.win.V.y;
/** world 和显示器一样大，气泡、菜单按显示器宽度排版（和以前铺满屏幕时一样） */
function sizeWorld() {
  worldEl.style.width = S.mon.w + 'px';
  worldEl.style.height = S.mon.h + 'px';
}

function toast(msg, ms = 1600) {
  toastEl.textContent = msg;
  toastEl.classList.add('show');
  S.toastUntil = now() + ms + 200;
  placeToast();
  clearTimeout(toast._t);
  toast._t = setTimeout(() => toastEl.classList.remove('show'), ms);
}

function placeToast() {
  if (now() > S.toastUntil) return;
  const b = bodyBox();
  toastEl.style.left = snap(S.pet.x) + 'px';
  toastEl.style.top = snap(Math.max(S.work.y + 4, b.top - 40)) + 'px';
}

// ───────────────────────── 形象包 ─────────────────────────

async function loadPack(name, base) {
  const root = name === 'placeholder' || !name ? 'sprites/placeholder/' : base + encodeURIComponent(name) + '/';
  const t0 = performance.now();
  let pack;
  try {
    pack = await (await fetch(root + 'pack.json', { cache: 'no-store' })).json();
  } catch (e) {
    console.warn('形象包加载失败，改用占位形象', e);
    if (name && name !== 'placeholder') return loadPack('placeholder', base);
    return;
  }
  S.decodeH = Math.ceil((S.cfg.pet_height || 240) * (window.devicePixelRatio || 1));
  await anim.load(root, pack, S.decodeH);
  // 待机素材读不出来，宠物就是透明的（0.2.1 遇到过“宠物没了”）：先换回占位形象，原因记进日志
  const a01 = anim.clips.A01;
  if (name && name !== 'placeholder' && a01 && !a01.ready) {
    const v = a01.video;
    console.warn(`形象包 ${name} 的待机素材读不出来，先用占位形象。` + (v ? `视频状态 readyState=${v.readyState} networkState=${v.networkState} error=${v.error ? v.error.code + ' ' + (v.error.message || '') : '无'}，能播 VP9：${v.canPlayType('video/webm; codecs="vp9"') || '否'}` : ''));
    return loadPack('placeholder', base);
  }
  S.aspect = anim.aspect();
  S.anchor = anim.norm(pack.anchor || [0.5, 1.0]) || [0.5, 1.0];
  // 角色实际范围：形象包里写了就用，没写就从待机素材的透明通道量
  const given = Array.isArray(pack.body) && pack.body.length === 4 ? [anim.norm(pack.body.slice(0, 2)), anim.norm(pack.body.slice(2))] : null;
  const body = given && given[0] && given[1] ? [...given[0], ...given[1]] : anim.measureBody();
  S.body = body && body[2] - body[0] > 0.05 && body[3] - body[1] > 0.05 ? body : [0, 0, 1, 1];
  S.eye = null;
  if (pack.eye) {
    const c = anim.norm([pack.eye.x, pack.eye.y]);
    const r = anim.norm([pack.eye.rx, pack.eye.ry]);
    if (c && r) S.eye = { x: c[0], y: c[1], rx: r[0], ry: r[1] };
  }
  for (const id of ['D02', 'F04']) petEl.classList.toggle('clip-' + id, anim.has(id));
  resizePet();
  diagLog('info', `形象包 ${name || 'placeholder'} 载入 ${Math.round(performance.now() - t0)}ms，有素材的动作 ${Object.keys(anim.clips).join(',') || '无'}，素材帧率 ${pack.fps || 30}，解码高度 ${S.decodeH}`);
}

// ───────────────────────── 诊断：每 30 秒记一次画面流畅度 ─────────────────────────

const diagLog = (level, msg) => { if (window.__diag) window.__diag.send(level, msg); };

// 视频画到画布上的检查结果写进日志（再出现“宠物透明”时能直接看出是哪一步）
anim.onProbe = async (r) => {
  let hw = '';
  try {
    const info = await navigator.mediaCapabilities.decodingInfo({
      type: 'file',
      video: { contentType: 'video/webm; codecs="vp09.00.10.08"', width: r.video[0] || 600, height: r.video[1] || 600, bitrate: 3000000, framerate: 60 },
    });
    hw = `；这台电脑解 VP9：支持 ${info.supported}，流畅 ${info.smooth}，省电(显卡解码) ${info.powerEfficient}`;
  } catch (e) { /* 老版本没有这个接口 */ }
  const pct = (r.frac * 100).toFixed(1) + '%';
  if (r.ok && r.frac > 0.97) diagLog('warn', `视频画面检查：整块都不透明（${pct}），透明背景没了，会看到一个方框${hw}`);
  else if (r.ok) diagLog('info', `视频画面检查：正常，画布 ${r.w}x${r.h} 里有 ${pct} 是角色${hw}`);
  else diagLog('warn', `视频画到画布上是透明的（角色占 ${pct}${r.err ? '，读画布出错 ' + r.err : ''}），改成直接显示视频${hw}`);
};
const PERF = { n: 0, long: 0, worst: 0, last: 0, since: 0, vTotal: 0, vDrop: 0 };

function perfTick(t) {
  if (PERF.last) {
    const dt = t - PERF.last;
    PERF.n++;
    if (dt > 50) PERF.long++;
    if (dt > PERF.worst) PERF.worst = dt;
  }
  PERF.last = t;
  if (!PERF.since) PERF.since = t;
  if (t - PERF.since < 30000) return;
  const secs = (t - PERF.since) / 1000;
  const sh = anim.shown || {};
  const media = sh.media;
  let video = '';
  if (media && media.getVideoPlaybackQuality) {
    const q = media.getVideoPlaybackQuality();
    const tot = q.totalVideoFrames - PERF.vTotal, drop = q.droppedVideoFrames - PERF.vDrop;
    PERF.vTotal = q.totalVideoFrames; PERF.vDrop = q.droppedVideoFrames;
    video = `，视频 ${media.videoWidth}x${media.videoHeight} 解出 ${tot} 帧 掉 ${drop} 帧`;
  }
  const W = S.win.V || {};
  diagLog('perf', `帧率 ${(PERF.n / secs).toFixed(1)}，卡顿帧(>50ms) ${PERF.long} 次，最慢一帧 ${Math.round(PERF.worst)}ms；动作 ${sh.id || '-'}${sh.css ? '(程序动画 ' + sh.css + ')' : ''}${video}；画布 ${canvas.width}x${canvas.height}，窗口 ${Math.round(W.w || 0)}x${Math.round(W.h || 0)}，缩放 ${window.devicePixelRatio}${S.walk ? '，正在跑' : ''}`);
  PERF.n = 0; PERF.long = 0; PERF.worst = 0; PERF.since = t;
}

/** 设置里的“大小”指角色本身有多高；素材四周留白时，整个画面要相应放大 */
function resizePet() {
  const want = S.cfg.pet_height || 240;
  const bh = S.body[3] - S.body[1];
  const h = Math.round(clamp(want / bh, want, want * 2.5));
  S.pet.h = h;
  S.pet.w = Math.round(h * S.aspect);
  petEl.style.width = S.pet.w + 'px';
  petEl.style.height = S.pet.h + 'px';
  const dpr = window.devicePixelRatio || 1;
  canvas.width = Math.round(S.pet.w * dpr);
  canvas.height = Math.round(S.pet.h * dpr);
  anim.invalidate();
  if (S.eye) {
    const f = $('#eye-flash');
    const r = S.eye.rx * S.pet.w * 1.2;
    f.style.width = f.style.height = r * 2 + 'px';
    f.style.left = S.eye.x * S.pet.w - r + 'px';
    f.style.top = S.eye.y * S.pet.h - r + 'px';
  }
  // 朝左镜像时以脚底为轴翻转，脚不会跟着挪位置
  flipEl.style.transformOrigin = `${S.anchor[0] * 100}% 100%`;
  anim.anchorX = S.anchor[0];
  // 影子、光环、思考小点、Zzz 跟着角色实际范围走
  const [x0, y0, x1] = S.body, bw = x1 - x0, foot = (1 - S.anchor[1]) * 100;
  const sh = $('#pet-shadow'), ring = $('#pet-ring');
  sh.style.left = (x0 + bw * 0.12) * 100 + '%';
  sh.style.right = (1 - x1 + bw * 0.12) * 100 + '%';
  sh.style.bottom = `calc(${foot}% - 6px)`;
  ring.style.left = (x0 - bw * 0.05) * 100 + '%';
  ring.style.right = (1 - x1 - bw * 0.05) * 100 + '%';
  ring.style.bottom = `calc(${foot}% - 10px)`;
  $('#think').style.top = Math.max(0, y0 * 100 - 4) + '%';
  $('#think').style.right = Math.max(0, (1 - x1) * 100 - 4) + '%';
  $('#zzz').style.top = y0 * 100 + 4 + '%';
  $('#zzz').style.right = (1 - x1) * 100 + 8 + '%';
  // 序列帧按现在的显示高度重新解码，保持清晰
  const need = Math.ceil(h * dpr);
  if (need > S.decodeH * 1.15) { S.decodeH = need; anim.decodeH = need; }
}

/** 画面里的横坐标镜像（以脚底为轴） */
const mirrorX = (x) => 2 * S.anchor[0] - x;

/** 角色实际范围在屏幕上的位置（朝左镜像时左右对调） */
function bodyBox() {
  const b = petBox();
  let [x0, y0, x1, y1] = S.body;
  if (anim._flip) [x0, x1] = [mirrorX(x1), mirrorX(x0)];
  return { left: b.left + x0 * b.w, top: b.top + y0 * b.h, w: (x1 - x0) * b.w, h: (y1 - y0) * b.h };
}

// ───────────────────────── 该播哪个动作 ─────────────────────────

/** 有事发生：醒过来，重新计算空闲时间 */
function markActive() {
  S.lastActive = now();
  S.nextIdle = Math.max(S.nextIdle, S.lastActive + 5000);
  if (S.sleep) {
    const was = S.sleep;
    S.sleep = null;
    petEl.classList.remove('sleeping');
    if (was === 'sleep') anim.play('F05');
    else anim.interrupt();
  }
}

/** 播一个单次动作（会叫醒宠物） */
function act(id, opts) {
  markActive();
  return anim.play(id, opts);
}

/** 底层动作：按优先级从高到低 */
function baseAnim(t) {
  if (S.drag && S.drag.moved) return 'B08';
  if (S.walk) return S.walk.air ? 'B06' : S.walk.skidding ? 'B04' : S.walk.mode === 'walk' ? 'B01' : 'B02';
  if (S.typing) return 'C06';
  if (S.pointing) return 'C02';
  // 帮你操作的间隙：侧身站着（停在急停的最后一帧），不来回转身
  if (S.op && anim.pose === 'side') return 'B04:end';
  if (S.alertUntil > t) return 'D01';
  if (S.flags.looking) return 'E01';
  if (S.speaking) return 'E03';
  if (S.flags.thinking) return 'D02';
  if (S.petting) return 'F06';
  if (S.flags.breathing) return 'E02';
  if (S.sleep === 'sleep') return 'F04';
  return 'A01';
}

// 随机待机小动作：A02/A03 最常见；空闲久了才伸懒腰、坐下、躺平
const IDLE_POOL = [['A02', 4], ['A03', 4], ['A04', 1], ['A05', 1], ['A06', 1], ['A07', 1], ['A08', 1], ['A09', 1], ['A10', 0], ['A11', 1], ['A12', 0]];

function pickIdle(idleFor) {
  const pool = IDLE_POOL.map(([id, w]) => {
    if (id === 'A04' && idleFor > 60000) w = 3;
    if (id === 'A10' && idleFor > 60000) w = 2;
    if (id === 'A12' && idleFor > 120000) w = 1;
    // 已经有素材的动作多播一点
    return [id, w * (anim.has(id) ? 3 : 1)];
  });
  let r = Math.random() * pool.reduce((a, [, w]) => a + w, 0);
  for (const [id, w] of pool) if ((r -= w) <= 0) return id;
  return 'A02';
}

function idleLife(t, base) {
  if (base !== 'A01' || anim.playing || S.walk || S.drag || S.menuOpen || S.op) return;
  const idleFor = t - S.lastActive;
  // 空闲 3 分钟：打瞌睡 → 睡觉
  if (!S.sleep && !S.flags.breathing && idleFor > 3 * 60 * 1000 && !bubbleEl.classList.contains('show')) {
    S.sleep = 'dozing';
    petEl.classList.add('sleeping');
    anim.play('F03').then((r) => { if (S.sleep === 'dozing' && !r.interrupted) S.sleep = 'sleep'; });
    return;
  }
  if (S.sleep || t < S.nextIdle) return;
  S.nextIdle = t + 8000 + Math.random() * 12000;
  const id = pickIdle(idleFor);
  anim.play(id, ANIM_CATALOG[id].loop ? { ms: 3000 + Math.random() * 2000 } : {});
}

/** 指着按钮时：每 3 秒回头看你一眼，8 秒你还没点就跺脚催 */
function pointLife(t) {
  const p = S.pointing;
  if (!p || anim.playing || S.walk) return;
  if (t > p.nextStomp) {
    p.nextStomp = t + 8000;
    p.nextLook = t + 3000;
    anim.play('C04');
  } else if (t > p.nextLook) {
    p.nextLook = t + 3000;
    anim.play('C03');
  }
}

// ───────────────────────── 位置与命中区域 ─────────────────────────

function petBox() {
  const left = S.pet.x - S.pet.w * S.anchor[0];
  const top = S.pet.y - S.pet.h * S.anchor[1];
  return { left, top, w: S.pet.w, h: S.pet.h };
}

function clampPet() {
  const w = S.work, half = ((S.body[2] - S.body[0]) * S.pet.w) / 2;
  S.pet.x = clamp(S.pet.x, w.x + half * 0.6, w.x + w.w - half * 0.6);
  S.pet.y = clamp(S.pet.y, w.y + (S.anchor[1] - S.body[1]) * S.pet.h * 0.6, w.y + w.h);
}

function placeBubble() {
  if (!bubbleEl.classList.contains('show')) return;
  const b = bodyBox();
  const bw = bubbleEl.offsetWidth, bh = bubbleEl.offsetHeight;
  let x = S.pet.x - bw / 2;
  x = snap(clamp(x, 8, S.mon.w - bw - 8));
  let y = b.top + b.h * 0.06 - bh - 10;
  if (y < 8) y = 8;
  y = snap(y);
  if (x !== S.bubbleX || y !== S.bubbleY) {
    S.bubbleX = x; S.bubbleY = y;
    bubbleEl.style.transform = `translate3d(${x}px, ${y}px, 0)`;
  }
  bubbleEl.style.setProperty('--tail', clamp(S.pet.x - x, 18, bw - 18) + 'px');
}

/** 某个元素在 world 里占的范围（getBoundingClientRect 是窗口坐标，加上窗口位置） */
function worldRect(el) {
  const r = el.getBoundingClientRect();
  return { x: r.left + S.win.V.x, y: r.top + S.win.V.y, w: r.width, h: r.height };
}

/** 窗口至少要包住的范围：宠物整张画面（加上影子、光环、蹦跳的余量）、气泡、菜单、提示 */
function needBox() {
  const b = petBox();
  let l = b.left - 18, t = b.top - 34, r = b.left + b.w + 18, bt = b.top + b.h + 24;
  const add = (x, y, w, h, m) => { l = Math.min(l, x - m); t = Math.min(t, y - m); r = Math.max(r, x + w + m); bt = Math.max(bt, y + h + m); };
  if (bubbleEl.classList.contains('show')) add(S.bubbleX, S.bubbleY, bubbleEl.offsetWidth, bubbleEl.offsetHeight + 8, 26);
  if (S.menuOpen) add(S.menuX, S.menuY, menuEl.offsetWidth, menuEl.offsetHeight, 26);
  if (now() < S.toastUntil) { const tw = toastEl.offsetWidth; add(snap(S.pet.x) - tw / 2, parseFloat(toastEl.style.top) || 0, tw, toastEl.offsetHeight + 10, 8); }
  return { l, t, r, b: bt };
}

/**
 * 摆窗口：窗口相对宠物脚底的偏移（rel）只在需要包住的东西变了时才改，
 * 宠物跑动时 rel 不变，只挪窗口，窗口里的画面一个像素都不动。
 */
function layoutWin(t) {
  const W = S.win, n = needBox();
  const px = S.pet.x, py = S.pet.y;
  const need = { l: n.l - px, t: n.t - py, r: n.r - px, b: n.b - py };
  const cur = W.rel;
  const fits = cur && need.l >= cur.l && need.t >= cur.t && need.r <= cur.r && need.b <= cur.b;
  const grow = (u) => ({ l: snapDown(u.l - 12), t: snapDown(u.t - 12), r: snapUp(u.r + 12), b: snapUp(u.b + 12) });
  if (!fits) {
    W.rel = grow(cur ? { l: Math.min(cur.l, need.l), t: Math.min(cur.t, need.t), r: Math.max(cur.r, need.r), b: Math.max(cur.b, need.b) } : need);
    W.shrinkAt = 0;
  } else if (!S.walk && !(S.drag && S.drag.moved)) {
    // 气泡、菜单收起后，停下来 1.5 秒再把窗口缩回去
    const area = (cur.r - cur.l) * (cur.b - cur.t), want = (need.r - need.l + 24) * (need.b - need.t + 24);
    if (area > want * 1.25) {
      if (!W.shrinkAt) W.shrinkAt = t + 1500;
      else if (t > W.shrinkAt) { W.rel = grow(need); W.shrinkAt = 0; }
    } else W.shrinkAt = 0;
  }
  const r = W.rel;
  const V = { x: snap(px + r.l), y: snap(py + r.t), w: r.r - r.l, h: r.b - r.t };
  if (V.x !== W.V.x || V.y !== W.V.y) worldEl.style.transform = `translate3d(${-V.x}px, ${-V.y}px, 0)`;
  W.V = V;
  const key = `${V.x},${V.y},${V.w},${V.h}`;
  if (key !== W.sent) {
    W.sent = key;
    invoke('win_place', { label: 'stage', x: V.x, y: V.y, w: V.w, h: V.h, show: W.shown ? null : true });
    W.shown = true;
  }
}

function sendRects(t, force) {
  if (!force && t - S.lastRectSend < 40) return;
  const rects = [];
  const b = bodyBox();
  // 宠物身体（按角色实际范围，四周再收一点，透明边缘不挡鼠标）
  rects.push({ x: b.left + b.w * 0.06, y: b.top + b.h * 0.03, w: b.w * 0.88, h: b.h * 0.95 });
  for (const el of [bubbleEl, menuEl]) {
    if (el.classList.contains('show')) {
      const r = worldRect(el);
      rects.push({ x: r.x - 4, y: r.y - 4, w: r.w + 8, h: r.h + 12 });
    }
  }
  const key = JSON.stringify(rects.map((r) => [r.x | 0, r.y | 0, r.w | 0, r.h | 0]));
  if (key !== S.lastRects || force) {
    S.lastRects = key;
    S.lastRectSend = t;
    invoke('set_hit_rects', { rects });
  }
}

function reportPet(t, force) {
  if (!force && t - S.lastReport < 250) return;
  S.lastReport = t;
  invoke('report_pet', { x: S.pet.x, y: S.pet.y });
}

// ───────────────────────── 走 / 跑 ─────────────────────────

function setFace(dir) {
  if (!dir) return;
  S.pet.face = dir > 0 ? 1 : -1;
  anim.setFace(S.pet.face);
}

function runSpeed() {
  return Math.max(300, (S.cfg.run_speed || 1800) / (window.devicePixelRatio || 1));
}

let travelSeq = 0;

function cancelTravel() {
  travelSeq++;
  if (S.walk) {
    const w = S.walk;
    S.walk = null;
    w.resolve(false);
  }
}

/**
 * 移动到 (x, y)（脚底中心，CSS 像素）。到了返回 true，被打断返回 false。
 * 跑：转身 → 加速 → 匀速 → 急停（急停动作和减速同时进行）；太远就闪现过去。
 */
async function travel(x, y, o = {}) {
  cancelTravel();
  const my = travelSeq;
  const run = o.mode !== 'walk';
  if (run) markActive();
  const sx = S.pet.x, sy = S.pet.y;
  const D = Math.hypot(x - sx, y - sy);
  const finish = () => {
    if (o.face) setFace(o.face === 'left' || o.face < 0 ? -1 : 1);
    reportPet(now(), true);
    sendRects(now(), true);
    return true;
  };
  if (D < 3) return finish();
  if (Math.abs(x - sx) > 4) setFace(x > sx ? 1 : -1);

  // 远距离或紧急：闪现（缩成一团消失 → 在目标处弹出来），不到半秒
  if (run && (D > 1500 || (o.urgent && D > 500))) {
    await anim.play('B09', { turn: false });
    if (my !== travelSeq) return false;
    S.pet.x = x;
    S.pet.y = y;
    reportPet(now(), true);
    await anim.play('B10', { turn: false });
    return my === travelSeq ? finish() : false;
  }
  // 正面 → 侧面：先转身（太近就不转了）
  if (D > 80 && anim.pose !== 'side' && anim.has('B03')) {
    await anim.play('B03', { turnTo: 'side' });
    if (my !== travelSeq) return false;
  }

  const air = run && Math.abs(y - sy) > S.pet.h * 0.8 && anim.has('B06');
  let T, pos, skidAt = null;
  if (!run) {
    T = clamp(D / 90, 0.5, 12);
    pos = (el) => D * clamp(el / T, 0, 1);
  } else {
    let v = o.speed || runSpeed();
    const ta = 0.12;
    const skidClip = !air && D > 120 && anim.has('B04');
    const ts = skidClip ? clamp(anim.duration('B04') / 1000, 0.3, 0.7) : 0.16;
    let tc = D / v - (ta + ts) / 2;
    // 匀速段最多 0.9 秒：再远就跑快点
    if (tc > 0.9) { v = D / (0.9 + (ta + ts) / 2); tc = 0.9; }
    if (tc < 0) { v = (2 * D) / (ta + ts); tc = 0; }
    T = ta + tc + ts;
    const s1 = (v * ta) / 2, s2 = s1 + v * tc;
    pos = (el) => {
      if (el <= 0) return 0;
      if (el < ta) return 0.5 * (v / ta) * el * el;
      if (el < ta + tc) return s1 + v * (el - ta);
      const k = Math.min(el - ta - tc, ts);
      return s2 + v * k - 0.5 * (v / ts) * k * k;
    };
    if (!air) skidAt = ta + tc;
  }
  anim.interrupt(); // 正在播的小动作让位给跑步
  const ok = await new Promise((resolve) => {
    S.walk = {
      sx, sy, x, y, D, T, pos, skidAt, air, mode: run ? 'run' : 'walk',
      hop: run && !air && !anim.has('B02'),
      skidding: false, t0: now(), resolve,
    };
  });
  if (!ok) return false;
  if (air) await anim.play('B07', { turn: false });
  return finish();
}

function stepWalk(t) {
  const w = S.walk;
  if (!w) return;
  const el = (t - w.t0) / 1000;
  const f = w.D ? clamp(w.pos(el) / w.D, 0, 1) : 1;
  S.pet.x = w.sx + (w.x - w.sx) * f;
  S.pet.y = w.sy + (w.y - w.sy) * f;
  // 占位形象没有跑步动作：跑的时候带一点弧线，看着像蹦过去
  if (w.hop) S.pet.y -= Math.sin(f * Math.PI) * Math.min(50, w.D * 0.08);
  if (w.skidAt !== null && !w.skidding && el >= w.skidAt) w.skidding = true;
  if (el >= w.T) {
    S.walk = null;
    w.resolve(true);
  }
}

function maybeWander(t) {
  if (!S.cfg.wander || S.walk || S.drag || S.menuOpen || S.op || S.pointing || S.sleep) return;
  if (S.speaking || S.flags.thinking || S.flags.operating || bubbleEl.classList.contains('show')) { S.nextWander = t + 8000; return; }
  if (t < S.nextWander) return;
  S.nextWander = t + 15000 + Math.random() * 30000;
  const w = S.work;
  const span = Math.min(500, w.w * 0.35);
  const x = clamp(S.pet.x + (Math.random() * 2 - 1) * span, w.x + S.pet.w, w.x + w.w - S.pet.w);
  travel(x, w.y + w.h, { mode: 'walk' });
}

/**
 * 跑到按钮旁边站哪儿：有指尖坐标时让指尖对准按钮（按 = 按钮中心，指 = 按钮边上不挡住它）。
 * 从哪边跑来就站哪边（不用跑过头再掉头），那边放不下就站另一边。
 */
function approachSpot(r, purpose) {
  const clipId = purpose === 'point' ? 'C02' : purpose === 'type' ? 'C06' : 'C01';
  const tip = anim.tip(clipId) || anim.tip('C01');
  const W = S.pet.w, H = S.pet.h, [ax, ay] = S.anchor, wa = S.work;
  const cy = r.y + r.h / 2;
  const target = (face) => (purpose === 'point' ? (face > 0 ? r.x - 6 : r.x + r.w + 6) : r.x + r.w / 2);
  const spotFor = (face) => {
    if (tip) {
      const dx = (tip[0] - ax) * W, dy = (tip[1] - ay) * H;
      return { x: face > 0 ? target(1) - dx : target(-1) + dx, y: cy - dy, face };
    }
    // 没有指尖坐标：身体贴着按钮站，身体上半部和按钮齐平
    const gap = 8, reach = (S.body[2] - ax) * W;
    const fy = cy + (ay - (S.body[1] + 0.4 * (S.body[3] - S.body[1]))) * H;
    return { x: face > 0 ? r.x - gap - reach : r.x + r.w + gap + reach, y: fy, face };
  };
  // 站在 sp 时身体左右边缘（朝左时镜像）
  const edges = (sp) => (sp.face > 0 ? [sp.x + (S.body[0] - ax) * W, sp.x + (S.body[2] - ax) * W] : [sp.x + (ax - S.body[2]) * W, sp.x + (ax - S.body[0]) * W]);
  const fits = (sp) => { const [l, rr] = edges(sp); return l >= wa.x + 4 && rr <= wa.x + wa.w - 4; };
  const halfBody = ((S.body[2] - S.body[0]) * W) / 2;
  const prefer = S.pet.x > r.x + r.w / 2 ? -1 : 1;
  let sp = spotFor(prefer);
  if (!fits(sp)) {
    const other = spotFor(-prefer);
    if (fits(other)) sp = other;
  }
  sp.x = clamp(sp.x, wa.x + halfBody * 0.6, wa.x + wa.w - halfBody * 0.6);
  sp.y = clamp(sp.y, wa.y + (ay - S.body[1]) * H * 0.6, wa.y + wa.h + H * 0.1);
  return sp;
}

// ───────────────────────── 组合动作 ─────────────────────────

function startPointing(p) {
  const t = now();
  markActive();
  S.pointing = { t0: t, nextLook: t + 3000, nextStomp: t + 8000 };
}

function endPointing(result) {
  if (!S.pointing) return;
  S.pointing = null;
  if (result === 'clicked') {
    if (S.op) S.op.celebrated = true;
    act('D03');
  } else if (result === 'timeout' || result === 'cancel') {
    act('C07');
  }
}

/** 操作做完：竖个大拇指，再跑回原来的位置 */
async function endOp(p) {
  const op = S.op;
  S.op = null;
  S.typing = false;
  if (S.pointing) endPointing(p.ok ? 'done' : 'cancel');
  if (!op) return;
  const moved = Math.hypot(S.pet.x - op.home.x, S.pet.y - op.home.y) > 40;
  if (p.ok && moved && !op.celebrated) await act('C05');
  if (!moved) return;
  await sleepMs(250);
  if (!S.op && !S.drag && !S.walk) travel(op.home.x, op.home.y);
}

// ───────────────────────── 主循环 ─────────────────────────

function frame(t) {
  perfTick(t);
  stepWalk(t);
  maybeWander(t);
  // 落在物理像素上：窗口位置和宠物画面一起取整，跑起来不会有亚像素抖动
  S.pet.x = snap(S.pet.x);
  S.pet.y = snap(S.pet.y);
  const base = baseAnim(t);
  if (base === 'B04:end') anim.setBase('B04', { atEnd: true });
  else anim.setBase(base, { turn: base !== 'B08' });
  idleLife(t, base);
  pointLife(t);
  anim.tick(t);
  const b = petBox();
  const bl = snap(b.left), bt = snap(b.top), pk = bl + ',' + bt;
  if (pk !== S.petKey) { S.petKey = pk; petEl.style.transform = `translate3d(${bl}px, ${bt}px, 0)`; }
  placeBubble();
  placeToast();
  layoutWin(t);
  // 气泡自动收起
  const bb = S.bubble;
  if (bubbleEl.classList.contains('show') && bb.done && !S.speaking && !bb.hover && bb.hideAt && t > bb.hideAt && !bubbleEl.querySelector('.btns button')) {
    hideBubble();
  }
  if (S.menuOpen && S.menuLeaveAt && t - S.menuLeaveAt > 700) closeMenu();
  sendRects(t, false);
  if (S.walk || (S.drag && S.drag.moved)) reportPet(t, false);
  requestAnimationFrame(frame);
}

// ───────────────────────── 气泡 ─────────────────────────

const TAGS = { breath: '🌬 呼吸模式', look: '👁 看了一眼', alert: '🔔 提醒', error: '⚠ 出错了', op: '🖐 帮你操作', talk: '' };

function showBubble(id, kind, text) {
  const bb = S.bubble;
  bb.id = id; bb.kind = kind || 'talk'; bb.done = false; bb.hideAt = 0;
  bubbleEl.className = 'show k-' + bb.kind;
  bubbleEl.querySelector('.tag').textContent = TAGS[bb.kind] || '';
  bubbleEl.querySelector('.tag').style.display = TAGS[bb.kind] ? 'block' : 'none';
  const tx = bubbleEl.querySelector('.text');
  tx.textContent = text || '';
  if (!text) tx.innerHTML = '<span class="cursor"></span>';
  bubbleEl.querySelector('.btns').innerHTML = '';
  S.lastActive = now();
  placeBubble();
  sendRects(now(), true);
}

function appendBubble(id, text) {
  if (S.bubble.id !== id) return;
  const tx = bubbleEl.querySelector('.text');
  const cur = tx.querySelector('.cursor');
  if (cur) cur.remove();
  tx.appendChild(document.createTextNode(text));
  tx.scrollTop = tx.scrollHeight;
}

function doneBubble(id) {
  if (id && S.bubble.id !== id) return;
  const bb = S.bubble;
  bb.done = true;
  const tx = bubbleEl.querySelector('.text');
  const cur = tx.querySelector('.cursor');
  if (cur) cur.remove();
  const n = tx.textContent.length;
  bb.hideAt = now() + clamp(2600 + n * 90, 3500, 15000);
}

function hideBubble() {
  if (S.bubble.kind === 'alert') S.alertUntil = 0;
  bubbleEl.classList.remove('show');
  S.bubble.id = null;
  sendRects(now(), true);
}

function setButtons(confirmId, buttons) {
  const box = bubbleEl.querySelector('.btns');
  box.innerHTML = '';
  for (const b of buttons) {
    const el = document.createElement('button');
    el.textContent = b.label;
    if (b.value) el.className = 'yes';
    el.onclick = (e) => {
      e.stopPropagation();
      box.querySelectorAll('button').forEach((x) => (x.disabled = true));
      invoke('confirm_reply', { id: confirmId, ok: !!b.value });
      setTimeout(() => { box.innerHTML = ''; doneBubble(); }, 150);
    };
    box.appendChild(el);
  }
  S.bubble.done = true;
  S.bubble.hideAt = now() + 30000;
}

bubbleEl.addEventListener('pointerenter', () => (S.bubble.hover = true));
bubbleEl.addEventListener('pointerleave', () => { S.bubble.hover = false; if (S.bubble.done) S.bubble.hideAt = Math.max(S.bubble.hideAt, now() + 1500); });
bubbleEl.addEventListener('click', (e) => { if (!e.target.closest('button') && !e.target.closest('.text')) hideBubble(); });
bubbleEl.addEventListener('dblclick', () => hideBubble());
// ───────────────────────── 点击 / 拖动 / 右键 ─────────────────────────

function inEye(cx, cy) {
  if (!S.eye) return false;
  const b = petBox();
  let lx = (cx - b.left) / b.w;
  const ly = (cy - b.top) / b.h;
  if (anim._flip) lx = mirrorX(lx);
  const dx = (lx - S.eye.x) / S.eye.rx, dy = (ly - S.eye.y) / S.eye.ry;
  return dx * dx + dy * dy <= 1;
}

let lastClick = 0;
// 拖动用屏幕坐标的差值（screenX/Y）：窗口跟着宠物在挪，窗口坐标会跟着变，不能用
bodyEl.addEventListener('pointerdown', (e) => {
  if (e.button !== 0) return;
  closeMenu();
  bodyEl.setPointerCapture(e.pointerId);
  S.drag = { sx: e.screenX, sy: e.screenY, px: S.pet.x, py: S.pet.y, moved: false, id: e.pointerId };
  markActive();
});

bodyEl.addEventListener('pointermove', (e) => {
  const d = S.drag;
  if (!d || d.id !== e.pointerId) {
    // 鼠标停在头上一会儿 = 摸头
    if (!S.drag) {
      const b = bodyBox();
      const onHead = (wy(e) - b.top) / b.h < 0.35;
      if (!onHead) { S.headSince = 0; S.petting = false; }
      else if (!S.headSince) S.headSince = now();
      else if (now() - S.headSince > 700 && !S.petting) { S.petting = true; markActive(); }
    }
    return;
  }
  const dx = e.screenX - d.sx, dy = e.screenY - d.sy;
  if (!d.moved && Math.hypot(dx, dy) > 5) {
    d.moved = true;
    cancelTravel();
    anim.interrupt();
    S.petting = false;
    petEl.classList.add('dragging');
    invoke('set_capture_all', { on: true });
  }
  if (d.moved) {
    const nx = d.px + dx;
    if (Math.abs(nx - S.pet.x) > 2) setFace(nx > S.pet.x ? 1 : -1);
    S.pet.x = nx;
    S.pet.y = d.py + dy;
  }
});

bodyEl.addEventListener('pointerleave', () => { S.headSince = 0; S.petting = false; });

bodyEl.addEventListener('pointerup', (e) => {
  const d = S.drag;
  if (!d || d.id !== e.pointerId) return;
  S.drag = null;
  petEl.classList.remove('dragging');
  if (d.moved) {
    clampPet();
    reportPet(now(), true);
    sendRects(now(), true);
    act('B07', { turn: false }); // 落地弹一下
    setTimeout(() => invoke('drag_end'), 30);
    return;
  }
  // 没拖动：算点击
  const t = now();
  if (t - lastClick < 320) {
    lastClick = 0;
    invoke('pet_clicked', { part: 'double' });
    return;
  }
  lastClick = t;
  if (inEye(wx(e), wy(e))) {
    const f = $('#eye-flash');
    f.classList.remove('on'); void f.offsetWidth; f.classList.add('on');
    act('E01');
    invoke('pet_clicked', { part: 'eye' });
  } else if (S.speaking || (S.bubble.id && !S.bubble.done)) {
    invoke('stop_all');
    toast('好，我闭嘴');
  } else if (S.alertUntil > t) {
    S.alertUntil = 0;
    act('F01');
  } else {
    // 连着戳好几下会生气
    S.pokes = S.pokes.filter((x) => t - x < 2500).concat(t);
    if (S.pokes.length >= 4) { S.pokes = []; act('F07'); }
    else act('F01');
  }
});

bodyEl.addEventListener('pointercancel', () => {
  if (S.drag && S.drag.moved) invoke('drag_end');
  S.drag = null;
  petEl.classList.remove('dragging');
});

bodyEl.addEventListener('contextmenu', (e) => {
  e.preventDefault();
  openMenu(wx(e), wy(e));
});
document.addEventListener('contextmenu', (e) => e.preventDefault());

// ───────────────────────── 右键菜单 ─────────────────────────

function menuHtml() {
  const f = S.flags, c = S.cfg;
  const mode = c.breath_mode || 'normal';
  const item = (act, ic, label, right = '') => `<div class="item" data-act="${act}"><span class="ic">${ic}</span><span>${label}</span>${right}</div>`;
  // 显示设置里实际生效的快捷键（被别的程序占了时会自动换成带 Shift 的）
  const kb = (key, dflt) => {
    const k = (S.cfg[key] || dflt).replace('CommandOrControl', 'Ctrl').replace('Space', '空格');
    return k ? `<span class="kb">${k}</span>` : '';
  };
  return [
    item('look', '👁', '看一眼', kb('hotkey_look', 'Ctrl+Alt+E')),
    item('breath', '🌬', f.breathing ? '关闭呼吸模式' : '开启呼吸模式', f.breathing ? '<span class="on">●</span>' : kb('hotkey_breath', 'Ctrl+Alt+B')),
    `<div class="seg">${[['quiet', '安静'], ['normal', '正常'], ['detailed', '细讲']].map(([k, n]) => `<span data-act="mode-${k}" class="${mode === k ? 'sel' : ''}">${n}</span>`).join('')}</div>`,
    item('watch', '🎯', '盯着刚才用的窗口', kb('hotkey_watch', 'Ctrl+Alt+W')),
    item('windows', '🪟', `选择要看的窗口…${f.targets ? `（${f.targets}）` : ''}`),
    '<div class="sep"></div>',
    item('operate', '🖐', '帮我操作…'),
    item('translate', '🔤', '框选翻译', kb('hotkey_translate', 'Ctrl+Alt+T')),
    item('chat', '💬', '对话框', kb('hotkey_chat', 'Ctrl+Alt+Space')),
    '<div class="sep"></div>',
    item('shutup', '🤐', '闭嘴 / 停手', kb('hotkey_stop', 'Ctrl+Alt+Q')),
    item('voice', c.tts_enabled ? '🔊' : '🔇', c.tts_enabled ? '关掉声音（只显示气泡）' : '打开声音'),
    item('wander', '🚶', '自己到处走走', c.wander ? '<span class="on">●</span>' : ''),
    item('settings', '⚙', '设置'),
    '<div class="sep"></div>',
    item('hide', '🙈', '藏起来（托盘图标可找回）'),
    item('quit', '⏻', '退出'),
  ].join('');
}

function openMenu(x, y) {
  menuEl.innerHTML = menuHtml();
  menuEl.classList.add('show');
  const w = menuEl.offsetWidth, h = menuEl.offsetHeight;
  const mx = snap(clamp(x + 4, 6, S.mon.w - w - 6));
  const my = snap(clamp(y - 10, 6, S.mon.h - h - 6));
  S.menuX = mx; S.menuY = my;
  menuEl.style.left = mx + 'px';
  menuEl.style.top = my + 'px';
  S.menuOpen = true;
  S.menuLeaveAt = 0;
  sendRects(now(), true);
}

function closeMenu() {
  if (!S.menuOpen) return;
  S.menuOpen = false;
  menuEl.classList.remove('show');
  sendRects(now(), true);
}

menuEl.addEventListener('pointerleave', () => (S.menuLeaveAt = now()));
menuEl.addEventListener('pointerenter', () => (S.menuLeaveAt = 0));
menuEl.addEventListener('click', (e) => {
  const el = e.target.closest('[data-act]');
  if (!el) return;
  const action = el.dataset.act;
  closeMenu();
  if (action === 'look') act('E01');
  invoke('menu_action', { action });
});
// ───────────────────────── 后端事件 ─────────────────────────

function applyConfig(c) {
  const oldH = S.cfg.pet_height, oldPack = S.cfg.sprite_pack;
  S.cfg = Object.assign({}, S.cfg, c);
  const needH = Math.ceil((S.cfg.pet_height || 240) * (window.devicePixelRatio || 1));
  if (oldPack !== undefined && oldPack !== S.cfg.sprite_pack) loadPack(S.cfg.sprite_pack, S.packBase);
  else if (needH > S.decodeH * 1.15) loadPack(S.cfg.sprite_pack, S.packBase); // 放大了很多：按新尺寸重新解码，保持清晰
  else if (oldH !== S.cfg.pet_height) { resizePet(); clampPet(); }
}

async function init() {
  const st = await invoke('get_state');
  if (st) {
    S.cfg = st.config;
    S.packBase = st.packBase;
    if (st.stage && st.stage.work) S.work = st.stage.work;
    if (st.stage && st.stage.mon) S.mon = st.stage.mon;
    sizeWorld();
    if (st.pet) { S.pet.x = st.pet.x; S.pet.y = st.pet.y; }
    S.flags.breathing = st.breathing;
  }
  // 宠物不在这块屏幕上（第一次启动或换了显示器）就放到右下角
  if (!(S.pet.x > S.work.x && S.pet.x < S.work.x + S.work.w && S.pet.y > S.work.y && S.pet.y <= S.work.y + S.work.h + 2)) {
    S.pet.x = S.work.x + S.work.w - S.pet.w * 0.8;
    S.pet.y = S.work.y + S.work.h;
  }
  clampPet();
  petEl.classList.toggle('breathing', !!S.flags.breathing);
  // 先跑第一帧把窗口摆好、显示出来，再读形象包：
  // 1. 窗口建出来时是隐藏的，隐藏的页面浏览器不给动画帧，0.2.0 等动画帧来显示窗口，有时就一直出不来；
  // 2. 隐藏的页面里视频可能不加载，0.2.1 先读形象包再显示，视频读不出来就一直卡着。
  frame(performance.now());
  try {
    await loadPack(S.cfg.sprite_pack || 'placeholder', S.packBase || '');
  } catch (e) {
    console.error('形象包加载出错，改用占位形象', e);
    await loadPack('placeholder', S.packBase || '').catch(() => {});
  }
  reportPet(now(), true);
  sendRects(now(), true);
  act('B11'); // 出场：从下面探出来
}

listen('bubble', (p) => {
  if (p.mode === 'hide') { hideBubble(); return; }
  if (p.mode === 'set') {
    markActive();
    if (p.kind === 'alert') S.alertUntil = now() + 15000;
    else if (p.kind === 'error') act('D04');
    // 关了气泡又开着声音时，普通讲话只出声；提醒、报错、带按钮的照样显示
    const mute = S.cfg.bubble_enabled === false && S.cfg.tts_enabled !== false && !p.buttons && p.kind !== 'alert' && p.kind !== 'error';
    if (mute) { S.mutedId = p.id; return; }
    showBubble(p.id, p.kind, p.text);
    if (p.buttons && p.confirm) setButtons(p.confirm, p.buttons);
  } else if (p.id && p.id === S.mutedId) return;
  else if (p.mode === 'append') appendBubble(p.id, p.text);
  else if (p.mode === 'done') doneBubble(p.id);
});
listen('bubble-clear-buttons', () => { bubbleEl.querySelector('.btns').innerHTML = ''; doneBubble(); });
listen('speaking', (p) => {
  S.speaking = !!(p && p.text);
  if (S.speaking) markActive();
  else if (S.bubble.done) S.bubble.hideAt = Math.max(S.bubble.hideAt, now() + 2500);
});
listen('flags', (f) => {
  const was = !!S.flags.breathing;
  S.flags = f;
  petEl.classList.toggle('breathing', !!f.breathing);
  petEl.classList.toggle('thinking', !!f.thinking);
  if (f.thinking || f.looking || f.operating) markActive();
  // 呼吸模式：开 = 眼睛亮起，关 = 倒放熄灭
  if (!!f.breathing !== was) act('E01', { reverse: !f.breathing });
  if (typeof f.breathMode === 'string') S.cfg.breath_mode = f.breathMode;
});
listen('config', (c) => applyConfig(c));
listen('walk-to', async (p) => {
  await travel(p.x, p.y, { speed: p.speed, face: p.face });
  if (p.id) invoke('walk_done', { id: p.id });
});
// 跑到按钮旁边（按 / 指 / 打字），到了告诉后端
listen('approach', async (p) => {
  const spot = approachSpot(p, p.purpose || 'press');
  await travel(spot.x, spot.y, { speed: p.speed, face: spot.face, urgent: p.urgent });
  if (p.id) invoke('walk_done', { id: p.id });
});
listen('pet-action', async (p) => {
  switch (p.action) {
    case 'press': {
      // 按下的那一帧告诉后端：后端这时才真的点下去
      let sent = false;
      const hit = () => { if (!sent) { sent = true; if (p.id) invoke('walk_done', { id: p.id }); } };
      await act('C01', { onHit: hit, turn: false });
      hit();
      if (p.double) await anim.play('C01', { turn: false });
      break;
    }
    case 'tap': act('C01', { turn: false }); break;
    case 'scroll': act('C09', { turn: false }); break;
    case 'point': act('C03'); break;
    case 'point-start': startPointing(p); break;
    case 'point-end': endPointing(p.result); break;
    case 'type-start': S.typing = true; markActive(); break;
    case 'type-end': S.typing = false; break;
    case 'op-begin': S.op = { home: { x: S.pet.x, y: S.pet.y }, celebrated: false }; markActive(); break;
    case 'op-end': endOp(p); break;
    case 'alert': S.alertUntil = now() + 15000; markActive(); break;
    case 'happy': act('D03'); break;
    case 'error': act('D04'); break;
    case 'startle': act('F02'); break;
    // 呼吸模式看到画面变了：开口之前先有个反应
    case 'notice': if (!S.walk && !S.op && !S.drag && !S.speaking && !S.pointing) act('D05'); break;
    default: break;
  }
});
listen('stage-geom', (p) => {
  if (p.work) S.work = p.work;
  if (p.mon) { S.mon = p.mon; sizeWorld(); }
  resizePet();
  // 换了显示器：从靠近的一边进场
  S.pet.y = S.work.y + S.work.h;
  clampPet();
  reportPet(now(), true);
});


init();
