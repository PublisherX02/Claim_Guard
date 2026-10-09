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
    decision_rejected: 'That decision was refused.', password_change_required: 'You must set a new password first.', not_implemented: 'This is not built yet.'
  };
  CG.api = function (method, path, body) {
    var headers = { 'Accept': 'application/json' };
    var init = { method: method, headers: headers, credentials: 'same-origin' };
    if (body !== undefined) { headers['Content-Type'] = 'application/json'; init.body = JSON.stringify(body); }
    if (method !== 'GET') headers['X-CSRF-Token'] = CG.cookie('cg_csrf');
    return fetch('/api/v1' + path, init).then(function (res) {
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
    }, function () { var err = new Error('Cannot reach the server.'); err.code = 'network'; throw err; });
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
  CG.stopTimers = function () { CG.state.timers.forEach(clearInterval); CG.state.timers = []; };

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

})();
