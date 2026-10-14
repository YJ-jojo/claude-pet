// 页面诊断：报错、警告、资源加载失败都写进 pet.log；页面加载好了报一声。
// 每个页面最先加载它，这样后面的脚本出错也能记下来。每分钟最多记 30 条，免得刷屏。
(function () {
  'use strict';
  const T = window.__TAURI__;
  const win = (location.pathname.split('/').pop() || 'page').replace(/\.html$/, '');
  const send = (level, msg) => {
    try { if (T) T.core.invoke('js_log', { win, level, msg: String(msg) }).catch(() => {}); } catch (e) { /* 忽略 */ }
  };
  const safe = (x) => {
    if (x instanceof Error) return x.stack || x.message;
    if (x && typeof x === 'object') { try { return JSON.stringify(x).slice(0, 300); } catch (e) { return String(x); } }
    return String(x);
  };
  let budget = 30, resetAt = Date.now() + 60000;
  const limited = (level, msg) => {
    const now = Date.now();
    if (now > resetAt) { budget = 30; resetAt = now + 60000; }
    if (budget-- > 0) send(level, msg);
  };
  window.addEventListener('error', (e) => {
    const t = e.target;
    if (t && t !== window && (t.src || t.href)) limited('warn', `资源加载失败 ${t.tagName} ${t.src || t.href}`);
    else if (e.message) limited('error', `${e.message} @${(e.filename || '').split('/').pop()}:${e.lineno}:${e.colno}`);
  }, true);
  window.addEventListener('unhandledrejection', (e) => limited('error', 'Promise 出错：' + safe(e.reason)));
  for (const lv of ['warn', 'error']) {
    const orig = console[lv].bind(console);
    console[lv] = (...a) => { orig(...a); limited(lv, a.map(safe).join(' ')); };
  }
  window.__diag = { send, win };
  const ready = () => send('ready', `（页面里 ${Math.round(performance.now())}ms）`);
  if (document.readyState === 'complete') ready();
  else window.addEventListener('load', ready, { once: true });
})();
