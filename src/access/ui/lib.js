/* ClaimGuard console: shared helpers.
 * Rule for this whole folder: text from the server (claim fields, AI explanations, notes, names) is untrusted. It is only ever put on
 * the page with textContent or text nodes, never as markup: no HTML-string sinks, no document writes, no dynamic code evaluation
 * anywhere (tests/test_ui_console.py scans every file for them). */
(function () {
  'use strict';
  var CG = window.CG = { state: { me: null, inbox: [], timers: [] } };

  // ---- DOM
  CG.h = function (tag, attrs) {
    var el = document.createElement(tag);
    if (attrs) {
      Object.keys(attrs).forEach(function (k) {
        var v = attrs[k];
        if (v === null || v === undefined || v === false) return;
        if (k === 'class') el.className = v;
        else if (k === 'text') el.textContent = v;
        else if (k.slice(0, 2) === 'on' && typeof v === 'function') el.addEventListener(k.slice(2), v);
        else if (k === 'value') el.value = v;
        else if (v === true) el.setAttribute(k, '');
        else el.setAttribute(k, String(v));
      });
    }
    for (var i = 2; i < arguments.length; i++) CG.append(el, arguments[i]);
    return el;
  };
  CG.append = function (el, child) {
    if (child === null || child === undefined || child === false) return;
    if (Array.isArray(child)) { child.forEach(function (c) { CG.append(el, c); }); return; }
    el.appendChild(child.nodeType ? child : document.createTextNode(String(child)));
  };
  CG.clear = function (el) { while (el.firstChild) el.removeChild(el.firstChild); return el; };
  CG.chip = function (label, kind) { return CG.h('span', { class: 'chip ' + (kind || label), text: String(label).replace(/_/g, ' ') }); };

  CG.table = function (columns, rows, opts) {
    opts = opts || {};
    var head = CG.h('tr', null, columns.map(function (c) { return CG.h('th', { class: c.num ? 'num' : '', text: c.label }); }));
    var body = rows.map(function (row) {
      var tr = CG.h('tr', { class: opts.onRow ? 'click' : '', tabindex: opts.onRow ? '0' : null }, columns.map(function (c) {
        var v = c.get(row);
        return CG.h('td', { class: c.num ? 'num' : '' }, v === null || v === undefined || v === '' ? CG.h('span', { class: 'muted', text: '-' }) : v);
      }));
      if (opts.onRow) {
        tr.addEventListener('click', function () { opts.onRow(row); });
        tr.addEventListener('keydown', function (e) { if (e.key === 'Enter') opts.onRow(row); });
      }
      return tr;
    });
    return CG.h('div', { class: 'tablewrap' }, CG.h('table', null, CG.h('thead', null, head), CG.h('tbody', null, body)));
  };

  CG.kv = function (pairs) {
    var dl = CG.h('dl', { class: 'kv' });
    pairs.forEach(function (p) { if (p[1] !== null && p[1] !== undefined && p[1] !== '') dl.appendChild(CG.h('dt', { text: p[0] })); if (p[1] !== null && p[1] !== undefined && p[1] !== '') dl.appendChild(CG.h('dd', null, p[1])); });
    return dl;
  };

  // ---- formatting
  CG.fmtValue = function (v) {
    if (v === null || v === undefined) return 'null';
    if (typeof v === 'object') return JSON.stringify(v);
    return String(v);
  };
  CG.fmtTime = function (secs) {
    if (!secs) return '-';
    var d = new Date(secs * 1000);
    return isNaN(d) ? '-' : d.toLocaleString();
  };
  CG.fmtDuration = function (secs) {
    secs = Math.max(0, Math.round(secs));
    var m = Math.floor(secs / 60), s = secs % 60;
    if (m >= 60) return Math.floor(m / 60) + 'h ' + (m % 60) + 'm';
    return m + 'm ' + (s < 10 ? '0' : '') + s + 's';
  };
  CG.money = function (n, cur) { return typeof n === 'number' ? n.toLocaleString(undefined, { maximumFractionDigits: 2 }) + (cur ? ' ' + cur : '') : '-'; };

  // ---- API
  CG.cookie = function (name) {
    var m = document.cookie.split('; ').filter(function (c) { return c.indexOf(name + '=') === 0; })[0];
    return m ? decodeURIComponent(m.slice(name.length + 1)) : '';
  };
  CG.ERRORS = {
    unauthenticated: 'Your session ended. Sign in again.', forbidden: 'You are not allowed to do that.', csrf: 'The page lost its security token. Reload and try again.',
    invalid_credentials: 'Sign-in failed. Check the badge, password and the current code.', too_many_attempts: 'Too many attempts. Wait and try again later.',
    conflict: 'Someone changed this first, or your claim lease ran out. Reload and look again.', not_found: 'That was not found.',
    invalid_request: 'The server refused that input.', unavailable: 'The database is not reachable right now. Try again in a moment.',
    refused: 'The file was refused.', busy: 'The reader is busy with other documents. Try again in a moment.', too_many_records: 'The file holds more records than can be read at once.', document_reader_unavailable: 'The document reader could not be reached.', payload_too_large: 'The file is larger than the 16 MB limit.',
    decision_rejected: 'That decision was refused.', wrong_code: 'That authenticator code is wrong, or was already used. Wait for the next code.', cross_site: 'That request came from another site and was refused.', password_change_required: 'You must set a new password first.', not_implemented: 'This is not built yet.'
  };
  CG.api = function (method, path, body) {
    var headers = { 'Accept': 'application/json' };
    var init = { method: method, headers: headers, credentials: 'same-origin' };
    if (body !== undefined) { headers['Content-Type'] = 'application/json'; init.body = JSON.stringify(body); }
    if (method !== 'GET') headers['X-CSRF-Token'] = CG.cookie('cg_csrf');
    return fetch('/api/v1' + path, init).then(function (res) { return settle(res, path); }, unreachable);
  };
  function settle(res, path) {
    return res.text().then(function (text) {
      var data = null;
      try { data = text ? JSON.parse(text) : null; } catch (e) { data = null; }
      if (res.ok) return data;
      var code = data && data.error ? data.error : 'http_' + res.status;
      var err = new Error(CG.ERRORS[code] || ('Request failed (' + code + ').'));
      err.status = res.status; err.code = code; err.data = data;
      if (res.status === 401 && path.indexOf('/auth/') !== 0) { CG.state.me = null; CG.stopTimers(); location.hash = '#/login'; }
      if (code === 'password_change_required') location.hash = '#/password';
      throw err;
    });
  }
  function unreachable() { var err = new Error('Cannot reach the server.'); err.code = 'network'; throw err; }
  // one file's raw bytes (a picture, a PDF, a JSON or FHIR file); the server reads the kind from the bytes, not from the name
  CG.upload = function (path, name, buffer, ctype) {
    var headers = { 'Accept': 'application/json', 'Content-Type': ctype || 'application/octet-stream', 'X-File-Name': String(name).slice(0, 80), 'X-CSRF-Token': CG.cookie('cg_csrf') };
    return fetch('/api/v1' + path, { method: 'POST', headers: headers, credentials: 'same-origin', body: buffer }).then(function (res) { return settle(res, path); }, unreachable);
  };

  // ---- feedback
  CG.toast = function (message, kind) {
    var old = document.getElementById('toast');
    if (old) old.remove();
    var el = CG.h('div', { id: 'toast', class: 'toast notice ' + (kind || 'info'), role: 'status', text: message });
    document.body.appendChild(el);
    setTimeout(function () { if (el.parentNode) el.remove(); }, kind === 'bad' ? 7000 : 3500);
  };
  CG.fail = function (err) { CG.toast(err && err.message ? err.message : 'Something went wrong.', 'bad'); };

  // ---- timers owned by the current page are cleared on every navigation
  CG.every = function (ms, fn) { CG.state.timers.push(setInterval(fn, ms)); };
  CG.stopTimers = function () {
    CG.state.timers.forEach(clearInterval); CG.state.timers = [];
    (CG.state.streams || []).forEach(function (s) { s.close(); }); CG.state.streams = [];
  };

  // ---- live updates: the server only says "changed" (no claim data); the page then re-reads through the usual routes. The timers stay as
  // the fallback, so a proxy that buffers the stream, or a browser without EventSource, only makes the page slower to notice, never wrong.
  CG.live = function (topics, onChange) {
    if (!window.EventSource) return;
    var es = new EventSource('/api/v1/work/events?topics=' + encodeURIComponent(topics)), pending = null, seenHello = false;
    function poke() { if (!pending) pending = setTimeout(function () { pending = null; onChange(); }, 300); }
    es.addEventListener('hello', function () { if (seenHello) poke(); seenHello = true; });   // after a reconnect, read once: changes during the gap were not announced
    es.addEventListener('changed', poke);
    es.addEventListener('end', function () { es.close(); });
    (CG.state.streams = CG.state.streams || []).push({ close: function () { es.close(); if (pending) clearTimeout(pending); } });
  };

  CG.has = function (perm) { return !!(CG.state.me && CG.state.me.permissions.indexOf(perm) >= 0); };

  CG.qr = function (text) {
    try {
      var q = window.qrcode(0, 'M');
      q.addData(text); q.make();
      return CG.h('img', { class: 'qr', alt: 'QR code for the authenticator app', src: q.createDataURL(5, 0) });
    } catch (e) { return null; }
  };

  // ---- small charts (inline SVG, no library, colours come from the stylesheet) -------------------------------------------
  var NS = 'http://www.w3.org/2000/svg';
  CG.svg = function (tag, attrs) {
    var el = document.createElementNS(NS, tag);
    if (attrs) Object.keys(attrs).forEach(function (k) { if (attrs[k] !== null && attrs[k] !== undefined) el.setAttribute(k, String(attrs[k])); });
    for (var i = 2; i < arguments.length; i++) if (arguments[i]) el.appendChild(arguments[i]);
    return el;
  };
  CG.svgText = function (attrs, text) { var t = CG.svg('text', attrs); t.textContent = text; return t; };

  /* A stacked bar chart. spec = { title, labels: [x labels], series: [{ name, values: [numbers], cls: 's0'..'s3' }], height, yLabel }.
   * Every bar carries a native tooltip (<title>), the SVG has a role and a text label, and the table of the same numbers is one click away
   * wherever the chart is used, so the picture is never the only way to read the data. */
  CG.barChart = function (spec) {
    var W = 640, H = spec.height || 150, L = 36, B = 18, T = 6, R = 4;
    var n = spec.labels.length, totals = spec.labels.map(function (_, i) { return spec.series.reduce(function (a, s) { return a + (s.values[i] || 0); }, 0); });
    var max = Math.max.apply(null, totals.concat([1]));
    var step = (W - L - R) / Math.max(1, n), bw = Math.max(1, step * 0.72);
    var svg = CG.svg('svg', { viewBox: '0 0 ' + W + ' ' + H, class: 'chart', role: 'img', 'aria-label': spec.title + '. Highest bar: ' + max + '.', preserveAspectRatio: 'none' });
    [0, 0.5, 1].forEach(function (f) {
      var y = T + (H - T - B) * (1 - f);
      svg.appendChild(CG.svg('line', { x1: L, x2: W - R, y1: y, y2: y, class: 'grid' }));
      svg.appendChild(CG.svgText({ x: L - 4, y: y + 3, class: 'axis', 'text-anchor': 'end' }, String(Math.round(max * f))));
    });
    spec.labels.forEach(function (label, i) {
      var y = H - B, x = L + i * step + (step - bw) / 2;
      spec.series.forEach(function (s) {
        var v = s.values[i] || 0;
        if (!v) return;
        var h = (H - T - B) * v / max;
        y -= h;
        svg.appendChild(CG.svg('rect', { x: x, y: y, width: bw, height: h, class: 'bar-' + s.cls }, (function () { var t = CG.svg('title'); t.textContent = label + ': ' + v + ' ' + s.name; return t; })()));
      });
    });
    [0, Math.floor((n - 1) / 2), n - 1].forEach(function (i) {
      if (i < 0 || i >= n) return;
      svg.appendChild(CG.svgText({ x: L + i * step + step / 2, y: H - 4, class: 'axis', 'text-anchor': i === 0 ? 'start' : i === n - 1 ? 'end' : 'middle' }, spec.labels[i]));
    });
    var legend = CG.h('div', { class: 'legend' }, spec.series.map(function (s) { return CG.h('span', null, CG.h('i', { class: 'swatch bar-' + s.cls }), ' ' + s.name); }));
    return CG.h('figure', { class: 'figure' }, svg, legend);
  };

  /* Horizontal bars for a few labelled counts: items = [{ label, value, cls }]. */
  CG.hbars = function (items, total) {
    var max = Math.max.apply(null, items.map(function (i) { return i.value; }).concat([1]));
    return CG.h('div', { class: 'hbars' }, items.map(function (it) {
      var fill = CG.h('i', { class: 'bar-' + (it.cls || 's0') });
      fill.style.width = (100 * it.value / max) + '%';
      return CG.h('div', { class: 'hbar' }, CG.h('span', { class: 'hl', text: String(it.label).replace(/_/g, ' ') }), CG.h('div', { class: 'bar' }, fill),
        CG.h('span', { class: 'hv', text: String(it.value) + (total ? ' (' + Math.round(100 * it.value / Math.max(1, total)) + '%)' : '') }));
    }));
  };

  CG.status = function (s) { return CG.h('span', { class: 'dot ' + s, role: 'img', 'aria-label': s }); };
  CG.when = function (iso) { var d = new Date(iso); return isNaN(d) ? String(iso || '-') : d.toLocaleString(); };

  // ---- notifications: the bell, the pop-ups (5 seconds each), and the detail window ----------------------------------------------------
  // The server keeps each person's notifications (claim arrived, information requested by the pipeline, lease expired). The live stream only
  // says "changed" (topic: notifications); this code then reads GET /work/notifications. The stream is opened once after sign-in and is not
  // one of the per-page streams, so moving between pages does not close it. A notification pops up once; a reload lists the old ones in the
  // bell without popping them again. Everything the server sends is put on the page as text only.
  var POPUP_MS = 5000, MAX_POPUPS = 4, FRESH_S = 30;
  var KINDS = { claim_arrived: ['New claim', 'info'], info_requested_auto: ['Pipeline action', 'ai'], lease_expired: ['Returned to pool', 'B'] };
  var N = CG.notes = { items: [], seen: null, es: null, timer: null, badge: null, bell: null, count: null, panel: null, pops: null, detail: null, started: false };

  function seenKey() { return 'cg_seen_' + (N.badge || ''); }
  function loadSeen() {
    var out = {};
    try { (JSON.parse(localStorage.getItem(seenKey()) || '[]') || []).forEach(function (id) { out[id] = true; }); } catch (e) { /* no storage: nothing is remembered */ }
    return out;
  }
  function saveSeen() { try { localStorage.setItem(seenKey(), JSON.stringify(Object.keys(N.seen).slice(-300))); } catch (e) { /* ignore */ } }

  function host() {
    if (N.pops) return;
    N.pops = CG.h('div', { class: 'note-pops', 'aria-live': 'polite' });
    document.body.appendChild(N.pops);
  }

  N.start = function () {
    var me = CG.state.me;
    if (!me || !CG.has('claims.decide')) { N.stop(); return; }
    if (N.started && N.badge === me.badge_id) return;
    N.stop();
    N.badge = me.badge_id; N.started = true; N.seen = null; N.items = [];
    host();
    N.refresh();
    if (window.EventSource) {
      var es = N.es = new EventSource('/api/v1/work/events?topics=notifications'), pending = null;
      var poke = function () { if (!pending) pending = setTimeout(function () { pending = null; N.refresh(); }, 200); };
      es.addEventListener('hello', poke); es.addEventListener('changed', poke);
      es.addEventListener('end', function () { es.close(); });
    }
    N.timer = setInterval(N.refresh, 20000);               // the fallback if the stream is buffered, closed after its five minutes, or unsupported
  };

  N.stop = function () {
    if (N.es) { N.es.close(); N.es = null; }
    if (N.timer) { clearInterval(N.timer); N.timer = null; }
    N.started = false; N.items = []; N.seen = null; N.badge = null;
    N.hidePanel(); N.closeDetail();
    if (N.pops) CG.clear(N.pops);
    N.paint();
  };

  N.refresh = function () {
    return CG.api('GET', '/work/notifications').then(function (r) {
      var rows = (r && r.notifications) || [];
      if (N.seen === null) N.seen = loadSeen();
      var now = Date.now() / 1000, fresh = [];
      rows.forEach(function (n) {
        if (N.seen[n.id]) return;
        N.seen[n.id] = true;
        if (now - n.at < FRESH_S) fresh.push(n);          // only what has just happened pops up; older ones wait in the bell
      });
      N.items = rows;
      saveSeen();
      fresh.reverse().forEach(N.popup);
      N.paint();
      if (N.panel) N.paintPanel();
    }, function (e) { if (e && e.status === 404) N.stop(); });
  };

  function kindOf(n) { return KINDS[n.kind] || ['Notice', 'neutral']; }
  function ago(at) {
    var s = Math.max(0, Math.round(Date.now() / 1000 - at));
    return s < 60 ? 'just now' : s < 3600 ? Math.floor(s / 60) + ' min ago' : s < 86400 ? Math.floor(s / 3600) + ' h ago' : new Date(at * 1000).toLocaleDateString();
  }

  N.popup = function (n) {
    host();
    var el = CG.h('div', { class: 'note-pop ' + (kindOf(n)[1]), role: 'status', tabindex: '0' },
      CG.h('div', { class: 'row between' }, CG.chip(kindOf(n)[0], kindOf(n)[1]), CG.h('button', { class: 'note-x', type: 'button', 'aria-label': 'Dismiss', text: 'x', onclick: function (e) { e.stopPropagation(); gone(); } })),
      CG.h('b', { text: n.title }), CG.h('div', { class: 'small muted', text: n.summary }), CG.h('i', { class: 'note-life' }));
    var timer = setTimeout(gone, POPUP_MS);
    function gone() { clearTimeout(timer); if (el.parentNode) el.remove(); }
    var open = function () { gone(); N.showDetail(n); };
    el.addEventListener('click', open);
    el.addEventListener('keydown', function (e) { if (e.key === 'Enter') open(); });
    N.pops.appendChild(el);
    while (N.pops.children.length > MAX_POPUPS) N.pops.removeChild(N.pops.firstChild);
  };

  // the bell in the sidebar: shell() rebuilds the page on every move, so it asks for a fresh one each time
  N.bellButton = function () {
    var count = CG.h('span', { class: 'note-count', 'aria-hidden': 'true' });
    var b = CG.h('button', { class: 'btn small note-bell', type: 'button', 'aria-label': 'Notifications', 'aria-expanded': 'false', onclick: function (e) { e.stopPropagation(); N.togglePanel(); } }, 'Notifications ', count);
    N.bell = b; N.count = count;
    N.paint();
    return b;
  };
  N.paint = function () {
    if (!N.count) return;
    var n = N.items.length;
    N.count.textContent = n ? String(n > 99 ? '99+' : n) : '';
    N.count.className = 'note-count' + (n ? ' on' : '');
    if (N.bell) N.bell.setAttribute('aria-label', n ? 'Notifications, ' + n : 'Notifications');
  };

  N.togglePanel = function () { if (N.panel) N.hidePanel(); else N.showPanel(); };
  N.hidePanel = function () {
    if (N.panel) { N.panel.remove(); N.panel = null; }
    if (N.bell) N.bell.setAttribute('aria-expanded', 'false');
  };
  N.showPanel = function () {
    N.panel = CG.h('div', { class: 'note-panel card', role: 'dialog', 'aria-label': 'Notifications' });
    document.body.appendChild(N.panel);
    if (N.bell) N.bell.setAttribute('aria-expanded', 'true');
    N.paintPanel();
    N.refresh();
  };
  N.paintPanel = function () {
    var p = N.panel; if (!p) return;
    CG.clear(p);
    var clear = CG.h('button', { class: 'btn small danger', type: 'button', text: 'Delete all', disabled: N.items.length ? null : true, onclick: N.clearAll });
    p.appendChild(CG.h('div', { class: 'row between' }, CG.h('b', { text: 'Notifications' }), CG.h('div', { class: 'row' }, clear, CG.h('button', { class: 'btn small', type: 'button', text: 'Close', onclick: N.hidePanel }))));
    if (!N.items.length) { p.appendChild(CG.h('p', { class: 'muted', text: 'Nothing new. Claims that reach you, and what the pipeline does by itself, will be listed here.' })); return; }
    p.appendChild(CG.h('ul', { class: 'plain note-list' }, N.items.map(function (n) {
      return CG.h('li', null, CG.h('button', { class: 'note-row', type: 'button', onclick: function () { N.showDetail(n); } },
        CG.chip(kindOf(n)[0], kindOf(n)[1]), CG.h('span', { class: 'note-title', text: n.title }), CG.h('span', { class: 'muted small', text: ago(n.at) })));
    })));
  };
  N.clearAll = function () {
    CG.api('POST', '/work/notifications/clear', {}).then(function () { N.items = []; N.paint(); N.paintPanel(); CG.toast('All notifications deleted.', 'good'); }, CG.fail);
  };

  N.closeDetail = function () { if (N.detail) { N.detail.remove(); N.detail = null; } };
  N.showDetail = function (n) {
    N.closeDetail();
    var body = CG.h('div', { class: 'stack' }, CG.h('div', { class: 'row between' }, CG.chip(kindOf(n)[0], kindOf(n)[1]), CG.h('span', { class: 'muted small', text: new Date(n.at * 1000).toLocaleString() })),
      CG.h('h2', { text: n.title }), CG.h('p', { text: n.summary }),
      CG.kv((n.lines || []).map(function (l) { return [String(l[0]), String(l[1])]; })));
    var actions = CG.h('div', { class: 'row' });
    if (n.kind === 'claim_arrived' && n.claim_id) {
      var open = CG.h('a', { class: 'btn primary', href: '#/work/' + encodeURIComponent(n.claim_id), text: 'Open the claim', onclick: function () { N.closeDetail(); N.hidePanel(); } });
      actions.appendChild(open);
      CG.api('GET', '/work/inbox').then(function (r) {          // do not offer a link that cannot open: the claim may have left this person's inbox
        if (!(r.claims || []).some(function (c) { return c.claim_id === n.claim_id; })) { open.remove(); actions.insertBefore(CG.h('span', { class: 'muted small', text: 'This claim is no longer in your inbox.' }), actions.firstChild); }
      }, function () {});
    }
    actions.appendChild(CG.h('button', { class: 'btn', type: 'button', text: 'Close', onclick: N.closeDetail }));
    body.appendChild(actions);
    N.detail = CG.h('div', { class: 'note-overlay', onclick: function (e) { if (e.target === N.detail) N.closeDetail(); } }, CG.h('div', { class: 'note-detail card', role: 'dialog', 'aria-modal': 'true', 'aria-label': n.title }, body));
    document.body.appendChild(N.detail);
  };
  document.addEventListener('keydown', function (e) { if (e.key === 'Escape') { if (N.detail) N.closeDetail(); else N.hidePanel(); } });
  document.addEventListener('click', function (e) { if (N.panel && !N.panel.contains(e.target) && !(N.bell && N.bell.contains(e.target)) && !(N.detail && N.detail.contains(e.target))) N.hidePanel(); });

})();
