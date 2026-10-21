// 动作播放器：按《动作设计表》的编号（A01…F07）播放形象包里的动作。
//
// 有素材就播素材（WebM 透明视频 / PNG 序列帧 / 图片），没有就用 CSS 程序动画顶上，
// 所以人物素材做好一条就能放进来一条，不用改代码。
//
// 播放约定（和动作设计表一致）：
//  - 动作之间切换有 100ms 淡入淡出，任何动作都能随时被打断；
//  - 正面系列（A/D/E/F）和侧面系列（B/C）之间，有 B03 转身素材时自动插一段转身；
//  - 侧面动作只做朝右的，朝左时程序镜像；
//  - C01 按下的那一帧（hit）通知程序真的点击，不等动画播完。
'use strict';

// pose: front 正面 / side 侧面 / turn 转身衔接
// css: 没素材时用的程序动画（stage.css 里的 .st-xxx）；ms: 程序动画单次时长，0 = 没素材就跳过
// alt: 没素材时先找的替补动作
const ANIM_CATALOG = {
  A01: { pose: 'front', css: 'idle', loop: true, name: '待机呼吸' },
  A02: { pose: 'front', css: 'tilt', ms: 900, name: '眨眼歪头' },
  A03: { pose: 'front', css: 'peek', ms: 1500, name: '东张西望' },
  A04: { pose: 'front', css: 'stretch', ms: 1700, name: '伸懒腰' },
  A05: { pose: 'front', css: 'scratch', ms: 1000, name: '挠头' },
  A06: { pose: 'front', css: 'spin', ms: 800, name: '转一圈' },
  A07: { pose: 'front', css: 'tidy', ms: 1200, name: '整理围巾' },
  A08: { pose: 'front', css: 'fiddle', ms: 1300, name: '玩中国结' },
  A09: { pose: 'front', css: 'hum', ms: 3000, loop: true, name: '哼歌摇摆' },
  A10: { pose: 'front', css: 'sit', ms: 2600, name: '坐下踢腿' },
  A11: { pose: 'front', css: 'kick', ms: 900, name: '踢小石子' },
  A12: { pose: 'front', css: 'lie', ms: 3000, name: '趴下躺平' },

  B01: { pose: 'side', css: 'walk', loop: true, alt: ['B02'], name: '走路' },
  B02: { pose: 'side', css: 'run', loop: true, alt: ['B01'], name: '跑步' },
  B03: { pose: 'turn', css: null, ms: 0, name: '转身' },
  B04: { pose: 'side', css: 'skid', ms: 160, name: '急停' },
  B05: { pose: 'side', css: null, ms: 0, name: '起跑' },
  B06: { pose: 'side', css: 'jump', ms: 0, hold: true, name: '跳起' },
  B07: { pose: 'side', css: 'land', ms: 300, name: '落地弹起' },
  B08: { pose: 'side', css: 'drag', loop: true, name: '被拖拽' },
  B09: { pose: 'side', css: 'vanish', ms: 200, name: '闪现消失' },
  B10: { pose: 'side', css: 'appear', ms: 260, name: '闪现出现' },
  B11: { pose: 'front', css: 'rise', ms: 600, alt: ['B10'], name: '探头出来' },

  C01: { pose: 'side', css: 'tap', ms: 380, hitMs: 120, name: '伸手按按钮' },
  C02: { pose: 'side', css: 'point', loop: true, name: '指向按钮' },
  C03: { pose: 'side', css: 'pointlook', ms: 900, alt: ['C02'], name: '指着回头看你' },
  C04: { pose: 'side', css: 'stomp', ms: 900, name: '跺脚催促' },
  C05: { pose: 'side', css: 'happy', ms: 900, alt: ['D03'], name: '竖大拇指' },
  C06: { pose: 'side', css: 'type', loop: true, name: '敲键盘' },
  C07: { pose: 'side', css: 'shrug', ms: 800, name: '耸肩' },
  C08: { pose: 'side', css: 'tap', ms: 600, name: '抓住拖' },
  C09: { pose: 'side', css: 'scroll', ms: 500, name: '滚动' },
  C10: { pose: 'side', css: 'look', loop: true, name: '拿放大镜' },

  D01: { pose: 'front', css: 'alert', loop: true, name: '需要你确认' },
  D02: { pose: 'front', css: 'think', loop: true, name: '思考中' },
  D03: { pose: 'front', css: 'happy', ms: 900, alt: ['C05'], name: '完成庆祝' },
  D04: { pose: 'front', css: 'sad', ms: 1200, alt: ['F02'], name: '出错' },
  D05: { pose: 'front', css: 'pop', ms: 400, name: '收到消息' },
  D06: { pose: 'front', css: 'bored', ms: 1600, name: '等得无聊' },

  E01: { pose: 'front', css: 'look', ms: 600, name: '眼睛亮起' },
  E02: { pose: 'front', css: 'scan', loop: true, name: '持续扫视' },
  E03: { pose: 'front', css: 'talk', loop: true, name: '说话讲解' },
  E04: { pose: 'front', css: 'scan', loop: true, alt: ['E02'], name: '拿望远镜' },

  F01: { pose: 'front', css: 'giggle', ms: 800, name: '被点到' },
  F02: { pose: 'front', css: 'startle', ms: 500, name: '吓一跳' },
  F03: { pose: 'front', css: 'doze', ms: 1600, name: '打瞌睡' },
  F04: { pose: 'front', css: 'sleep', loop: true, name: '睡觉' },
  F05: { pose: 'front', css: 'wake', ms: 700, name: '醒来' },
  F06: { pose: 'front', css: 'purr', loop: true, name: '被摸头' },
  F07: { pose: 'front', css: 'angry', ms: 800, name: '生气鼓脸' },
};

// 第一版形象包里可能用的旧名字
const ANIM_ALIAS = {
  idle: 'A01', walk: 'B01', run: 'B02', turn: 'B03', skid: 'B04', land: 'B07', drag: 'B08',
  tap: 'C01', press: 'C01', point: 'C02', thumbs: 'C05', type: 'C06', shrug: 'C07',
  alert: 'D01', think: 'D02', happy: 'D03', error: 'D04',
  look: 'E01', breath: 'E02', scan: 'E02', talk: 'E03',
  tickle: 'F01', startle: 'F02', doze: 'F03', sleep: 'F04', wake: 'F05',
};

const MAX_DECODED_FRAMES = 1400; // 序列帧解码后的缓存上限（按帧数算），超了就丢最久没用的动作

class PetAnim {
  constructor(els) {
    this.body = els.body;
    this.flipEl = els.flip;
    this.img = els.img;
    this.canvas = els.canvas;
    this.prev = els.prev;
    // 画布放在内存里画（willReadFrequently）：0.2.1 在 Windows 上用显卡画透明视频，画出来整片是透明的，
    // 同一份视频用内存画布画是正常的（量身体范围那一步量出来了）
    this.ctx = this.canvas.getContext('2d', { willReadFrequently: true });
    this.pctx = this.prev.getContext('2d', { willReadFrequently: true });
    this.vbox = document.createElement('div');
    // 视频放在屏幕左上角一个 2 像素的透明小盒子里（完全放到屏幕外的话浏览器可能暂停解码）
    this.vbox.style.cssText = 'position:fixed;left:0;top:0;width:2px;height:2px;overflow:hidden;opacity:0;pointer-events:none;z-index:-1';
    document.body.appendChild(this.vbox);
    // 视频画到画布上还是透明时，改成直接显示视频元素（direct）
    this.direct = false;
    // 待机呼吸由程序每帧算（不用 CSS 动画）：换到别的动作时幅度慢慢收回 1，不会一下子缩放跳一下
    this.breath = { amp: 0, on: false, t: 0, t0: 0 };
    for (const el of [this.canvas, this.img]) el.style.transformOrigin = '50% 100%';
    this.probe = { n: 0, done: false };
    this.onProbe = null;
    this.reset();
  }

  reset() {
    for (const c of Object.values(this.clips || {})) if (c.video) { c.video.pause(); c.video.remove(); if (c.alt) c.alt.remove(); if (c.rev) c.rev.remove(); }
    for (const u of this.objUrls || []) URL.revokeObjectURL(u);
    this.objUrls = [];
    this.clips = {};
    this.root = '';
    this.size = null; // 素材原始尺寸 [w, h]（用来换算像素坐标）
    this.baseReq = { id: 'A01', opts: {} };
    this.shot = null;
    this.shown = null; // {key, id, clip, css, start, flip, media}
    this.pose = 'front';
    this.face = 1;
    this.decodeH = 300;
    this.lru = [];
    this.decodedFrames = 0;
    this.turning = false;
  }

  // ───────── 载入形象包 ─────────

  async load(root, pack, decodeH) {
    this.reset();
    this.root = root;
    this.decodeH = decodeH || 300;
    this.size = Array.isArray(pack.size) ? pack.size : null;
    const fps = pack.fps || 30;
    for (const [key, def] of Object.entries(pack.actions || {})) {
      const id = ANIM_CATALOG[key] ? key : ANIM_ALIAS[key];
      if (!id || this.clips[id]) continue;
      const meta = ANIM_CATALOG[id];
      const clip = {
        id,
        fps: def.fps || fps,
        loop: def.loop !== undefined ? def.loop : !!meta.loop,
        hit: def.hit,
        tip: def.tip || null,
        // 走路片：cycle=[起, 止] 帧号之间是能首尾相接的一步一步（之前是起步，之后是收脚站稳），
        // speed=原速播放时每秒走多少个身高，ease=[起步开始, 起步走稳, 开始收脚, 站稳] 帧号
        range: Array.isArray(def.cycle) && def.cycle.length === 2 && def.cycle[1] > def.cycle[0] ? def.cycle.map((f) => f / fps) : null,
        speed: def.speed > 0 ? def.speed : 0,
        ease: Array.isArray(def.ease) && def.ease.length === 4 ? def.ease.map((f) => f / fps) : null,
        loops: 0, stopping: false,
        w: 0, h: 0,
        ready: false,
      };
      if (def.video) {
        clip.kind = 'video';
        // 循环动作用两个视频轮流播：浏览器自带的 loop 回到开头时要跳转，会停顿 3、4 帧；
        // 一个播完马上换另一个（已经停在第一帧），接缝处不卡
        clip.video = this._video(root + def.video);
        if (clip.loop === true) clip.alt = this._video(root + def.video);
        clip.cur = clip.video;
        if (def.reverse) clip.rev = this._video(root + def.reverse);
        clip.video.addEventListener('loadeddata', () => {
          clip.w = clip.video.videoWidth; clip.h = clip.video.videoHeight; clip.ready = true;
        }, { once: true });
        clip.video.addEventListener('error', () => console.warn('动作视频播不了', id, def.video), { once: true });
      } else if (def.frames || (def.pattern && def.count)) {
        clip.kind = 'frames';
        clip.urls = (def.frames || this._pattern(def)).map((u) => root + u);
        clip.bitmaps = null;
      } else if (def.src) {
        clip.kind = 'img';
        clip.url = root + def.src;
        // 动图（gif / webp / apng）自己会动；静态图靠程序动画
        clip.animated = def.animated !== undefined ? !!def.animated : /\.(gif|webp|apng)$/i.test(def.src);
        clip.el = new Image();
        clip.loading = this._src(clip.el, clip.url)
          .then((u) => { clip.url = u; return clip.el.decode(); })
          .then(() => { clip.w = clip.el.naturalWidth; clip.h = clip.el.naturalHeight; clip.ready = true; })
          .catch(() => console.warn('动作图片读不了', id, def.src));
      } else continue;
      this.clips[id] = clip;
    }
    // 最常用的几条先解码好，其它第一次用到时再解码
    const first = ['A01', 'E03', 'D02', 'B02', 'C01', 'C02', 'E02'];
    await this._ensure('A01');
    first.slice(1).forEach((id) => this._ensure(id));
    this.shown = null;
  }

  _pattern(def) {
    const urls = [];
    const start = def.start || 1;
    for (let i = 0; i < def.count; i++) {
      const n = String(start + i);
      urls.push(def.pattern.replace(/%0(\d)d/, (_, w) => n.padStart(+w, '0')).replace('%d', n));
    }
    return urls;
  }

  /**
   * 先把素材整个读进内存再交给 <img>/<video>：不依赖跨域头和分段下载，
   * 画到 canvas 上也不会被“污染”（量身体范围要读像素）。读不了就直接用原地址。
   */
  _src(el, url) {
    return fetch(url)
      .then((r) => (r.ok ? r.blob() : Promise.reject(new Error('HTTP ' + r.status))))
      .then((b) => {
        const u = URL.createObjectURL(b);
        this.objUrls.push(u);
        el.src = u;
        return u;
      })
      .catch(() => { el.src = url; return url; });
  }

  _video(url) {
    const v = document.createElement('video');
    this._src(v, url);
    v.muted = true;
    v.playsInline = true;
    v.preload = 'auto';
    if (this.direct) this._directStyle(v);
    this.vbox.appendChild(v);
    // 视频出了新的一帧才重画画布（显示器 144Hz 时不用每秒转换 144 次）
    v._frameAt = 0;
    v._fresh = true;
    if (v.requestVideoFrameCallback) {
      const onFrame = () => { v._frameAt = performance.now(); v._fresh = true; v.requestVideoFrameCallback(onFrame); };
      v.requestVideoFrameCallback(onFrame);
    }
    return v;
  }

  _directStyle(v) {
    v.style.cssText = 'position:absolute;inset:0;width:100%;height:100%;object-fit:contain;object-position:50% 0;display:none';
  }

  /** 视频画到画布上是透明的：改成把视频元素本身摆在宠物的位置上 */
  _goDirect() {
    if (this.direct) return;
    this.direct = true;
    this.vbox.style.cssText = 'position:absolute;inset:0;pointer-events:none';
    this.canvas.parentNode.insertBefore(this.vbox, this.canvas);
    for (const c of Object.values(this.clips)) {
      for (const v of [c.video, c.alt, c.rev]) if (v) this._directStyle(v);
    }
    this.shown = null; // 下一帧按新方式重新摆
  }

  /** 第 10 次画视频时检查一次画布：一点不透明的像素都没有，就是这台电脑上画不出来 */
  _checkCanvas(src) {
    const p = this.probe;
    if (p.done || ++p.n < 10) return;
    p.done = true;
    const W = this.canvas.width, H = this.canvas.height;
    let op = 0, n = 0, err = '';
    try {
      const d = this.ctx.getImageData(0, 0, W, H).data;
      for (let i = 3; i < d.length; i += 16) { n++; if (d[i] > 32) op++; }
    } catch (e) { err = String(e); }
    const frac = n ? op / n : 0;
    const ok = !err && frac > 0.002;
    if (!ok) this._goDirect();
    if (this.onProbe) this.onProbe({ ok, frac, err, w: W, h: H, video: [src.videoWidth, src.videoHeight], direct: this.direct });
  }

  /** 确保某个动作的素材已经可以播（序列帧要先解码） */
  _ensure(id) {
    const c = this.clips[id];
    if (!c) return Promise.resolve(false);
    if (c.ready) { this._touch(id); return Promise.resolve(true); }
    if (c.kind === 'img') return (c.loading || Promise.resolve()).then(() => c.ready);
    if (c.kind === 'video') {
      if (c.video.readyState >= 2) { c.w = c.video.videoWidth; c.h = c.video.videoHeight; c.ready = true; return Promise.resolve(true); }
      // 最多等 4 秒：视频读不出来又不报错时（比如解码器不支持），不能让整个形象包一直卡在这里
      return new Promise((res) => {
        let timer = 0;
        const done = (ok) => { clearTimeout(timer); res(ok); };
        timer = setTimeout(() => done(c.video.readyState >= 2), 4000);
        c.video.addEventListener('loadeddata', () => done(true), { once: true });
        c.video.addEventListener('error', () => done(false), { once: true });
      });
    }
    if (!c.decoding) {
      const h = Math.round(this.decodeH);
      c.decoding = Promise.all(c.urls.map((u) => fetch(u)
        .then((r) => r.blob())
        .then((b) => createImageBitmap(b, { resizeHeight: h, resizeQuality: 'high' }))
        .catch(() => null)))
        .then(async (bms) => {
          bms = bms.filter(Boolean);
          if (!bms.length) return false;
          // 原始尺寸：解第一帧原图量一下（坐标换算要用）
          try {
            const raw = await createImageBitmap(await (await fetch(c.urls[0])).blob());
            c.w = raw.width; c.h = raw.height; raw.close();
          } catch (e) { c.w = bms[0].width; c.h = bms[0].height; }
          c.bitmaps = bms;
          c.ready = true;
          this.decodedFrames += bms.length;
          this._touch(id);
          this._evict();
          return true;
        })
        .finally(() => { c.decoding = null; });
    }
    return c.decoding;
  }

  _touch(id) {
    const i = this.lru.indexOf(id);
    if (i >= 0) this.lru.splice(i, 1);
    this.lru.push(id);
  }

  _evict() {
    while (this.decodedFrames > MAX_DECODED_FRAMES && this.lru.length > 2) {
      const id = this.lru.find((x) => x !== 'A01' && (!this.shown || this.shown.id !== x) && this.clips[x] && this.clips[x].kind === 'frames');
      if (!id) break;
      this.lru.splice(this.lru.indexOf(id), 1);
      const c = this.clips[id];
      c.bitmaps.forEach((b) => b.close());
      this.decodedFrames -= c.bitmaps.length;
      c.bitmaps = null;
      c.ready = false;
    }
  }

  // ───────── 查询 ─────────

  /**
   * 量出角色在画面里实际占的范围 [x0, y0, x1, y1]（0..1），用待机素材的透明通道算。
   * 素材四周留白时，宠物大小、能点到的范围、气泡位置都按这个范围算。
   */
  measureBody() {
    const c = this.clips.A01;
    if (!c || !c.ready) return null;
    const src = c.kind === 'img' ? c.el : c.kind === 'video' ? c.video : c.bitmaps && c.bitmaps[0];
    if (!src) return null;
    const sw = src.naturalWidth || src.videoWidth || src.width, sh = src.naturalHeight || src.videoHeight || src.height;
    if (!sw || !sh) return null;
    const H = 160, W = Math.max(1, Math.round((sw * H) / sh));
    const cv = document.createElement('canvas');
    cv.width = W;
    cv.height = H;
    const x = cv.getContext('2d', { willReadFrequently: true });
    let d;
    try {
      x.drawImage(src, 0, 0, W, H);
      d = x.getImageData(0, 0, W, H).data;
    } catch (e) { return null; }
    let x0 = W, y0 = H, x1 = -1, y1 = -1;
    for (let yy = 0; yy < H; yy++) {
      for (let xx = 0; xx < W; xx++) {
        if (d[(yy * W + xx) * 4 + 3] > 32) {
          if (xx < x0) x0 = xx;
          if (xx > x1) x1 = xx;
          if (yy < y0) y0 = yy;
          if (yy > y1) y1 = yy;
        }
      }
    }
    if (x1 < 0) return null;
    return [x0 / W, y0 / H, (x1 + 1) / W, (y1 + 1) / H];
  }

  /** 形象包里有没有这个动作的素材（不管解码好没有） */
  has(id) { return !!this.clips[id]; }

  /** 素材的宽高比（以待机动作为准） */
  aspect() {
    const c = this.clips.A01;
    const w = (this.size && this.size[0]) || (c && c.w), h = (this.size && this.size[1]) || (c && c.h);
    return w && h ? w / h : 1;
  }

  /** 像素坐标 → 0..1 的比例坐标（形象包里写比例或像素都行） */
  norm(pt, clip) {
    if (!Array.isArray(pt)) return null;
    if (pt[0] <= 1.5 && pt[1] <= 1.5) return pt;
    const w = (this.size && this.size[0]) || (clip && clip.w) || (this.clips.A01 && this.clips.A01.w);
    const h = (this.size && this.size[1]) || (clip && clip.h) || (this.clips.A01 && this.clips.A01.h);
    return w && h ? [pt[0] / w, pt[1] / h] : null;
  }

  /** 指尖位置（C 组动作），没有就返回 null */
  tip(id) {
    const c = this.clips[id];
    return c && c.tip ? this.norm(c.tip, c) : null;
  }

  /** 某个动作单次播放要多久（毫秒）；没素材时是程序动画的时长 */
  duration(id) {
    const v = this._resolve(id);
    if (!v) return 0;
    if (v.css !== null) return v.ms;
    return this._clipMs(v.clip);
  }

  _clipMs(c) {
    if (c.kind === 'frames') return (c.bitmaps.length / c.fps) * 1000;
    if (c.kind === 'video') return (c.video.duration || 1) * 1000;
    return 0;
  }

  /**
   * 找能播的东西：素材 → 替补素材 → 程序动画。
   * 程序动画叠在哪张图上：侧面动作且宠物正侧着身（在播侧面素材）时，叠在当前侧面画面上，
   * 免得突然跳回正面；其它情况叠在待机素材上。
   */
  _resolve(id) {
    const meta = ANIM_CATALOG[id] || ANIM_CATALOG.A01;
    for (const cand of [id, ...(meta.alt || [])]) {
      const c = this.clips[cand];
      if (c) {
        if (c.ready && (c.kind !== 'img' || c.animated)) return { id: cand, clip: c, css: null, pose: ANIM_CATALOG[cand].pose };
        if (!c.ready) this._ensure(cand);
      }
    }
    // 静态图片当素材时（比如占位形象只有一张图），动作靠程序动画
    const own = this.clips[id];
    if (own && own.ready && own.kind === 'img' && !own.animated) return { id, clip: own, css: meta.css || 'idle', ms: meta.ms || 0, pose: 'front' };
    if (!meta.css && !meta.loop && !meta.hold) return null; // 转身、起跑这类：没素材就跳过
    const sideHost = (meta.pose === 'side' || meta.pose === 'turn') && this.pose === 'side' && this.shown && this.shown.css === null &&
      this.shown.clip.kind !== 'img' && ANIM_CATALOG[this.shown.clip.id].pose === 'side' ? this.shown.clip : null;
    if (sideHost) return { id, clip: sideHost, css: meta.css || 'idle', ms: meta.ms || 0, pose: 'side' };
    const base = this.clips.A01;
    if (!base || !base.ready) return null;
    return { id, clip: base, css: meta.css || 'idle', ms: meta.ms || 0, pose: 'front' };
  }

  /** 要不要先转身：目标是正面/侧面且和现在不一样，并且有转身素材 */
  _needTurn(v) {
    const tp = v && v.pose;
    return (tp === 'front' || tp === 'side') && tp !== this.pose && v.id !== 'B03' && this.clips.B03 && this.clips.B03.ready ? tp : null;
  }

  _turnShot(to, then) {
    return { id: 'B03', v: this._resolve('B03'), opts: { reverse: to === 'front', turnTo: to }, t0: null, hitDone: false, resolve: then };
  }

  // ───────── 控制 ─────────

  /**
   * 设置“底层”动作：没有单次动作在播时一直播它（循环；不循环的停在最后一帧）。
   * opts.atEnd 直接停在最后一帧（比如急停之后侧身站着）；opts.turn=false 不自动插转身
   */
  setBase(id, opts = {}) {
    if (this.baseReq.id === id && !!this.baseReq.opts.atEnd === !!opts.atEnd && !opts.restart) return;
    this.baseReq = { id, opts };
  }

  /**
   * 播一次某个动作，播完（或被打断）时 resolve。
   * opts.reverse 倒放；opts.onHit 播到按下帧时回调；opts.ms 循环动作播多久；opts.turn=false 不自动插转身
   */
  play(id, opts = {}) {
    this.interrupt();
    const v = this._resolve(id);
    if (!v || (v.css !== null && !v.ms && !opts.ms)) {
      if (opts.onHit) opts.onHit();
      return Promise.resolve({ skipped: true });
    }
    // 单次动作却只找到循环素材（替补）：按设计表的时长播
    if (v.css === null && v.clip.loop && !opts.ms && !(ANIM_CATALOG[id] || {}).loop && v.id !== id) {
      opts = Object.assign({}, opts, { ms: (ANIM_CATALOG[id] || {}).ms || 900 });
    }
    return new Promise((resolve) => {
      const shot = { id, v, opts, t0: null, hitDone: false, resolve };
      const to = opts.turn !== false && !opts.turnTo ? this._needTurn(v) : null;
      if (!to) { this.shot = shot; return; }
      // 先转身再做动作
      this.shot = this._turnShot(to, (r) => {
        if (r.interrupted) {
          if (opts.onHit) opts.onHit();
          resolve(r);
        } else this.shot = shot;
      });
    });
  }

  /** 打断正在播的单次动作 */
  interrupt() {
    const s = this.shot;
    if (!s) return;
    this.shot = null;
    if (s.opts.onHit && !s.hitDone) { s.hitDone = true; s.opts.onHit(); }
    s.resolve({ interrupted: true });
  }

  get playing() { return this.shot ? this.shot.id : null; }

  /**
   * 底层循环视频是不是刚回到开头（或马上回到开头）。待机小动作在这时切进去，
   * 首帧和待机首帧是同一个姿势，接得上；回来时待机也从开头接着播。不是视频时总是 true。
   */
  atLoopStart(win = 0.06) {
    const s = this.shown;
    if (!this.shot && s && s.css === 'idle' && this.breath.on) return this._breathPhase(this.breath.t - this.breath.t0) < 0.03;
    if (this.shot || !s || !s.media) return true;
    if (!s.clip.loop) return false; // 单次动作刚播完、还停在最后一帧，等下一帧换回待机
    const m = s.media, d = m.duration;
    if (!(d > 0) || m.paused) return true;
    return m.currentTime < win || m.currentTime > d - win / 2;
  }

  /** 画布尺寸变了，下一帧强制重画 */
  invalidate() { this._lastClip = null; }

  setFace(dir) { if (dir) this.face = dir > 0 ? 1 : -1; }

  // ───────── 每帧 ─────────

  tick(t) {
    // 1. 要显示什么
    let want, shot = this.shot;
    if (shot) want = shot.v;
    else {
      want = this._resolve(this.baseReq.id) || this._resolve('A01');
      // 正面 ↔ 侧面：有转身素材就先转个身
      const to = this.baseReq.opts.turn !== false ? this._needTurn(want) : null;
      if (to) {
        this.shot = shot = this._turnShot(to, () => {});
        want = shot.v;
      }
    }
    if (!want) return;

    const reverse = !!(shot && shot.opts.reverse);
    const atEnd = !shot && !!this.baseReq.opts.atEnd;
    const key = `${want.id}|${want.css}|${reverse}|${shot ? 's' + shot.id : 'b' + this.baseReq.id}|${atEnd}`;
    const reqPose = (ANIM_CATALOG[shot ? shot.id : this.baseReq.id] || {}).pose;
    const flip = this.face < 0 && (reqPose === 'side' || reqPose === 'turn');

    // 2. 换动作：上一帧画面淡出 100ms
    if (!this.shown || this.shown.key !== key) {
      const prevShown = this.shown;
      const media = this._media(want.clip, reverse);
      // 同一份素材上只是换了程序动画：素材接着播，不从头开始，也不用淡出
      const sameClip = !!(prevShown && prevShown.clip === want.clip && (want.css !== null || prevShown.css !== null) && prevShown.media === media);
      if (prevShown && !(sameClip && want.css !== null && prevShown.css !== null)) this._crossfade(prevShown);
      if (prevShown && prevShown.media && prevShown.media !== media) {
        prevShown.media.pause();
        // 单次动作的视频先倒回开头，下次播放不用等跳转
        if (!prevShown.clip.loop) try { prevShown.media.currentTime = 0; } catch (e) { /* 忽略 */ }
        else this._skipLoopEnd(prevShown.clip, prevShown.media);
      }
      let start = sameClip ? prevShown.start : t;
      if (atEnd && want.css === null) start = t - this._clipMs(want.clip) - 1;
      this.shown = { key, id: want.id, clip: want.clip, css: want.css, start, media };
      if (media && !sameClip) {
        // 循环动作接着上次的位置播（跳转时会空一两帧）；单次动作从头播
        try {
          if (atEnd) media.currentTime = Math.max(0, (media.duration || 0) - 0.02);
          else if (!want.clip.loop && media.currentTime > 0.001) media.currentTime = 0;
        } catch (e) { /* 还没加载好 */ }
      }
      // 从静态图切到画布：先把图画上去，视频第一帧出来之前不会空白
      if (!this._onImg(want.clip) && prevShown && this._onImg(prevShown.clip) && this.img.naturalWidth) {
        const W = this.canvas.width, H = this.canvas.height, dw = (this.img.naturalWidth * H) / this.img.naturalHeight;
        this.ctx.clearRect(0, 0, W, H);
        this.ctx.drawImage(this.img, (W - dw) / 2, 0, dw, H);
        this._lastClip = null;
      }
      if (media && !atEnd) media.play().catch(() => {});
      // 停在最后一帧的底层动作，没素材时就别再演一遍程序动画了
      const css = atEnd && want.css !== null ? 'idle' : want.css;
      const b = this.breath;
      // 停下来以后重新开始呼吸：从原样开始吸气，不接着上次停在半截的地方
      if (css === 'idle' && !b.on && b.amp < 0.05) b.t0 = t;
      b.on = css === 'idle';
      this.body.className = css && css !== 'idle' ? 'st-' + css : 'has-frames';
      const direct = this.direct && want.clip.kind === 'video';
      if (this.direct) {
        for (const c of Object.values(this.clips)) {
          for (const v of [c.video, c.alt, c.rev]) if (v) v.style.display = direct && v === media ? 'block' : 'none';
        }
      }
      if (this._onImg(want.clip)) {
        this.canvas.style.display = 'none';
        this.img.style.display = 'block';
        if (this.img.getAttribute('src') !== want.clip.url) this.img.src = want.clip.url;
      } else {
        this.img.style.display = 'none';
        this.canvas.style.display = direct ? 'none' : 'block';
      }
      if (shot && shot.t0 === null) shot.t0 = t;
      if (want.pose === 'front' || want.pose === 'side') this.pose = want.pose;
    }
    if (flip !== this._flip) {
      this._flip = flip;
      this.flipEl.classList.toggle('left', flip);
    }

    // 3. 单次动作的进度：按下帧回调、播完收尾
    if (shot) {
      if (shot.t0 === null) shot.t0 = t;
      const el = t - shot.t0;
      const dur = shot.opts.ms || (want.css !== null ? want.ms : this._clipMs(want.clip));
      const hitAt = this._hitMs(shot, want, dur);
      if (shot.opts.onHit && !shot.hitDone && el >= hitAt) { shot.hitDone = true; shot.opts.onHit(); }
      const endless = want.css === null && want.clip.loop && !shot.opts.ms;
      // 视频单次动作按视频真的播完来算：开始播放有几十毫秒延迟，按时钟算会在最后一帧上停一下
      const m = this.shown.media;
      const vidEnd = want.css === null && !shot.opts.ms && m && !reverse && want.clip.kind === 'video';
      const over = vidEnd ? m.ended || el >= dur + 1000 : el >= dur;
      if (!endless && over) {
        // 这一帧保持最后的画面，下一帧再换：让接下来的动作（比如转身后开跑）有机会先排上
        this.shot = null;
        if (shot.opts.turnTo) this.pose = shot.opts.turnTo;
        shot.resolve({ done: true });
        return;
      }
    }

    // 4. 画当前帧
    this._breathe(t);
    this._loopSwap();
    this._draw(want.clip, t - this.shown.start, reverse, !!shot, this.shown.media);
  }

  _hitMs(shot, want, dur) {
    if (want.css !== null) return Math.min(dur, (ANIM_CATALOG[shot.id] || {}).hitMs || 0);
    const c = want.clip;
    if (typeof c.hit === 'number') return (c.hit / c.fps) * 1000;
    return dur * 0.45;
  }

  _media(clip, reverse) {
    if (!clip || clip.kind !== 'video') return null;
    return reverse && clip.rev ? clip.rev : clip.cur || clip.video;
  }

  /**
   * 循环视频被别的动作接走：下次回来从开头接。每段动作都是从这个站姿开始、回到这个站姿结束的，
   * 接着上次停下的地方播会先跳一下（比如转身回来、眨完眼回来）。
   * 换成停在开头备用的那个视频，不用等跳转
   */
  _skipLoopEnd(c, m) {
    if (!c.alt || m === c.rev) return;
    c.cur = m === c.video ? c.alt : c.video;
    try { if (c.cur.currentTime > 0.001) c.cur.currentTime = 0; } catch (e) { /* 还没加载好 */ }
    try { m.currentTime = 0; } catch (e) { /* 忽略 */ }
  }

  /** 循环视频播到头：换成另一个停在第一帧的视频接着播，播完的这个倒回开头备用 */
  _loopSwap() {
    const s = this.shown, c = s && s.clip, m = s && s.media;
    if (!m || !c.alt || m === c.rev || m.paused && !m.ended) return;
    // 走路片在一步一步那段里循环；要收脚时不再跳回去，播到最后站稳的那一帧停住
    const cyc = c.range && !c.stopping;
    if (c.range && c.stopping) return;
    const end = cyc ? c.range[1] : m.duration;
    if (!m.ended && !(end > 0 && m.currentTime >= end - 0.5 / c.fps)) return;
    const start = cyc ? c.range[0] : 0;
    const next = m === c.video ? c.alt : c.video;
    try { if (Math.abs(next.currentTime - start) > 0.001) next.currentTime = start; } catch (e) { /* 还没加载好 */ }
    next.playbackRate = m.playbackRate;
    next.play().catch(() => {});
    c.cur = s.media = next;
    if (cyc) c.loops++;
    if (this.direct) { next.style.display = 'block'; m.style.display = 'none'; }
    m.pause();
    try { m.currentTime = start; } catch (e) { /* 忽略 */ }
  }

  // ───────── 走路：按片子里真实的脚步算位置 ─────────

  /** 这个动作能不能按脚步走（片子里标了 cycle 和 speed） */
  canStep(id) {
    const c = this.clips[id];
    return !!(c && c.kind === 'video' && c.range && c.speed && c.alt && c.ready);
  }

  /** 从头开始走：两个视频都倒回去（第二个停在循环起点备用），按 rate 倍速播 */
  stepStart(id, rate) {
    const c = this.clips[id];
    c.loops = 0;
    c.stopping = false;
    c.cur = c.video;
    for (const v of [c.video, c.alt]) { v.pause(); v.playbackRate = rate; }
    try { c.video.currentTime = 0; c.alt.currentTime = c.range[0]; } catch (e) { /* 还没加载好 */ }
    if (this.shown && this.shown.clip === c) this.shown.media = c.video;
  }

  /** 走完当前这一圈就收脚站稳 */
  stepStop(id) { const c = this.clips[id]; if (c) c.stopping = true; }

  /** 走路片展开后的播放位置（秒）：第一遍从 0 算起，之后每绕一圈加一圈的长度；播完返回 null 之外的 done */
  stepTime(id) {
    const c = this.clips[id], m = c && (c.cur || c.video);
    if (!m || !c.range) return { u: 0, loops: 0, done: true };
    const [a, b] = c.range, t = m.currentTime;
    const u = c.loops === 0 ? t : b + (c.loops - 1) * (b - a) + (t - a);
    return { u, loops: c.loops, done: c.stopping && (m.ended || (m.duration > 0 && t >= m.duration - 0.5 / c.fps)) };
  }

  /** 走完了：速度恢复正常，下次从头开始 */
  stepEnd(id) {
    const c = this.clips[id];
    if (!c) return;
    c.stopping = false;
    for (const v of [c.video, c.alt]) if (v) v.playbackRate = 1;
  }

  _draw(c, elapsed, reverse, isShot, media) {
    if (this._onImg(c)) return;
    const W = this.canvas.width, H = this.canvas.height;
    let src = null;
    if (c.kind === 'img') {
      // 静态图和视频画在同一块画布上：缩放、位置、边缘都一样，换动作时看不出是两层
      if (this._lastClip === c) return;
      src = c.el;
    } else if (c.kind === 'video') {
      if (this.direct) return;
      src = media || c.video;
      if (src.readyState < 2) return;
      // 视频没出新的一帧就不重画（新一帧的通知 0.25 秒没来过，说明这台电脑上不发通知，那就每帧都画）
      const live = performance.now() - src._frameAt < 250;
      if (live && !src._fresh && this._lastClip === c && this._lastSrc === src) return;
      src._fresh = false;
      this._lastSrc = src;
    } else if (c.kind === 'frames' && c.bitmaps) {
      const n = c.bitmaps.length;
      let i = Math.floor((elapsed * c.fps) / 1000);
      if (c.loop === 'pingpong' && n > 1) {
        const period = 2 * n - 2;
        i %= period;
        if (i >= n) i = period - i;
      } else if (c.loop && !isShot) i %= n;
      else if (c.loop) i %= n;
      else i = Math.min(i, n - 1);
      if (reverse) i = n - 1 - i;
      if (i === c._last && this._lastClip === c) return;
      c._last = i;
      src = c.bitmaps[i];
    }
    if (!src) return;
    this._lastClip = c;
    const sw = src.videoWidth || src.naturalWidth || src.width, sh = src.videoHeight || src.naturalHeight || src.height;
    if (!sw || !sh) return;
    const dh = H, dw = (sw * H) / sh;
    this.ctx.clearRect(0, 0, W, H);
    // 素材是 600 像素的大图，缩小时用高质量缩放，不然边缘发糊、有锯齿
    this.ctx.imageSmoothingQuality = 'high';
    this.ctx.drawImage(src, (W - dw) / 2, 0, dw, dh);
    if (c.kind === 'video') this._checkCanvas(src);
  }

  _crossfade(prevShown) {
    const p = this.prev, src = this._onImg(prevShown.clip) ? this.img : this.canvas;
    p.width = this.canvas.width;
    p.height = this.canvas.height;
    const ctx = this.pctx;
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.imageSmoothingQuality = 'high';
    ctx.clearRect(0, 0, p.width, p.height);
    // 和 #pet-flip 一样以脚底为轴镜像
    if (this._flip) ctx.setTransform(-1, 0, 0, 1, 2 * (this.anchorX ?? 0.5) * p.width, 0);
    try {
      if (src === this.img) {
        const iw = this.img.naturalWidth, ih = this.img.naturalHeight;
        if (iw && ih) {
          const dw = (iw * p.height) / ih;
          ctx.drawImage(this.img, (p.width - dw) / 2, 0, dw, p.height);
        }
      } else ctx.drawImage(this.canvas, 0, 0);
    } catch (e) { return; }
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    if (this._fade) this._fade.cancel();
    this._fade = p.animate([{ opacity: 1 }, { opacity: 0 }], { duration: 100, easing: 'linear', fill: 'forwards' });
    if (!this._onImg(prevShown.clip)) this._lastClip = null;
  }

  /** 只有动图（gif / webp / apng）用 <img> 显示，它自己会动；其它都画在画布上 */
  _onImg(c) { return c.kind === 'img' && c.animated; }

  /** 呼吸进度 0（原样）→ 1（吸到最满），2.8 秒一次 */
  _breathPhase(t) { return (1 - Math.cos((2 * Math.PI * t) / 2800)) / 2; }

  /** 待机呼吸：和以前的 CSS 动画一样的幅度；开、关时幅度在 0.15 秒左右里渐变 */
  _breathe(t) {
    const b = this.breath, dt = Math.min(100, Math.max(0, t - (b.t || t)));
    b.t = t;
    b.amp += ((b.on ? 1 : 0) - b.amp) * (1 - Math.exp(-dt / 150));
    if (!b.on && b.amp < 0.002) b.amp = 0;
    const k = b.amp * this._breathPhase(t - b.t0);
    const tf = k ? `translateY(${k.toFixed(3)}px) scale(${(1 + 0.012 * k).toFixed(5)}, ${(1 - 0.015 * k).toFixed(5)})` : '';
    if (tf === b.tf) return;
    b.tf = tf;
    this.canvas.style.transform = tf;
    this.img.style.transform = tf;
  }
}

window.PetAnim = PetAnim;
window.ANIM_CATALOG = ANIM_CATALOG;
