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
      var cls = (opts.onRow ? 'click' : '') + (opts.rowClass ? ' ' + (opts.rowClass(row) || '') : '');
      var tr = CG.h('tr', { class: cls.trim(), tabindex: opts.onRow ? '0' : null }, columns.map(function (c) {
        var v = c.get(row);
        // data-label lets the phone layout show each cell with its column name (style.css turns rows into cards under 640 px)
        return CG.h('td', { class: c.num ? 'num' : '', 'data-label': c.label || '' }, v === null || v === undefined || v === '' ? CG.h('span', { class: 'muted', text: '-' }) : v);
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

  // ---- icons: small stroke glyphs drawn as SVG paths (decorative; the text next to them carries the meaning) --------------
  var ICONS = {
    home: 'M3 11l9-7 9 7 M5 10v10h14V10 M10 20v-6h4v6',
    inbox: 'M3 13h5l2 3h4l2-3h5 M5 5h14l2 8v6H3v-6z',
    list: 'M8 6h13 M8 12h13 M8 18h13 M3.5 6h.01 M3.5 12h.01 M3.5 18h.01',
    book: 'M4 19V5a2 2 0 012-2h13v14H6a2 2 0 00-2 2zm0 0a2 2 0 002 2h13',
    grid: 'M3 3h7v7H3z M14 3h7v7h-7z M3 14h7v7H3z M14 14h7v7h-7z',
    upload: 'M12 16V4 M7 9l5-5 5 5 M4 20h16',
    send: 'M22 2L11 13 M22 2l-7 20-4-9-9-4z',
    layers: 'M12 3l9 5-9 5-9-5z M3 13l9 5 9-5',
    sliders: 'M4 6h9 M17 6h3 M4 12h3 M11 12h9 M4 18h11 M19 18h1 M15 4v4 M9 10v4 M17 16v4',
    scale: 'M12 3v18 M5 7h14 M5 7l-3 6h6z M19 7l-3 6h6z M8 21h8',
    pulse: 'M3 12h4l3-8 4 16 3-8h4',
    shield: 'M12 3l8 3v6c0 5-3.5 8-8 9-4.5-1-8-4-8-9V6z M9 12l2 2 4-4',
    users: 'M16 20v-2a4 4 0 00-4-4H6a4 4 0 00-4 4v2 M9 10a4 4 0 100-8 4 4 0 000 8z M22 20v-2a4 4 0 00-3-3.9 M16 2.1a4 4 0 010 7.8',
    palette: 'M12 3a9 9 0 100 18c1 0 1.5-.7 1.5-1.5 0-.4-.2-.8-.4-1.1-.3-.3-.4-.6-.4-1 0-.8.7-1.4 1.5-1.4H16a5 5 0 005-5c0-4.4-4-8-9-8z M7.5 11h.01 M10 7.5h.01 M14.5 7.5h.01',
    menu: 'M4 6h16 M4 12h16 M4 18h16',
    x: 'M6 6l12 12 M18 6L6 18',
    check: 'M20 6L9 17l-5-5',
    clock: 'M12 7v5l3 2 M12 21a9 9 0 100-18 9 9 0 000 18z',
    eye: 'M2 12s3.5-7 10-7 10 7 10 7-3.5 7-10 7S2 12 2 12z M12 15a3 3 0 100-6 3 3 0 000 6z',
    logout: 'M9 21H5a2 2 0 01-2-2V5a2 2 0 012-2h4 M16 17l5-5-5-5 M21 12H9',
    star: 'M12 3l2.7 5.6 6.1.9-4.4 4.3 1 6.1L12 17l-5.4 2.9 1-6.1L3.2 9.5l6.1-.9z',
    lock: 'M5 11h14v10H5z M8 11V7a4 4 0 018 0v4',
    bolt: 'M13 2L4 14h7l-1 8 9-12h-7z',
    bell: 'M6 8a6 6 0 0 1 12 0c0 7 3 9 3 9H3s3-2 3-9 M10.3 21a1.94 1.94 0 0 0 3.4 0',
    image: 'M3 5h18v14H3z M3 16l5-5 4 4 3-3 6 6 M15.5 9.5h.01',
    volume: 'M11 5L6 9H2v6h4l5 4z M15.5 8.5a5 5 0 0 1 0 7 M19 5a10 10 0 0 1 0 14'
  };
  CG.icon = function (name) {
    var svg = CG.svg('svg', { viewBox: '0 0 24 24', class: 'ic', 'aria-hidden': 'true', focusable: 'false' });
    svg.appendChild(CG.svg('path', { d: ICONS[name] || ICONS.list }));
    return svg;
  };

  // ---- the rulebook's names, so a reviewer reads "R003 Coverage active on service date" and never has to memorise numbers
  CG.RULES = {
    R001: 'Required claim information', R002: 'Service and submission chronology', R003: 'Coverage active on service date',
    R004: 'Member and beneficiary consistency', R005: 'Provider in the supplied network', R006: 'Possible duplicate service lines',
    R007: 'Line arithmetic', R008: 'Required authorization reference', R009: 'Authorization record matches service',
    R010: 'Required supporting document', R011: 'Service code in fictional catalogue', R012: 'Claim total equals line amounts',
    R013: 'Quantity and price limits', R014: 'Submission window', R015: 'Currency matches policy',
    E001: 'Procedure pair without exception modifier', E002: 'Exception modifier on a never-bundle pair', E003: 'Event date required by the diagnosis',
    E004: 'Route of administration missing', E005: 'Diagnosis cannot be primary', E101: 'Line duplicates an earlier claim',
    E102: 'Authorization exhausted across claims', E103: 'Daily quantity across claims', E104: 'Invoice number reused',
    E105: 'Many claims in a short time', E106: 'Same service at another provider'
  };
  CG.ruleName = function (id) { return CG.RULES[id] || ''; };

  // "/lines/0/service_date" -> "Line L1 · service date": the JSON pointer stays visible beside it (it is the audit evidence), this only helps reading
  CG.pathLabel = function (path, claim) {
    var p = String(path || '').split('/').filter(Boolean), lines = (claim && claim.lines) || [];
    if (!p.length) return 'Whole claim';
    function words(s) { return String(s).replace(/_/g, ' '); }
    if ((p[0] === 'lines' || p[0] === 'attachments' || p[0] === 'authorizations') && /^\d+$/.test(p[1] || '')) {
      var i = +p[1], name = p[0] === 'lines' ? 'Line ' + ((lines[i] && lines[i].line_id) || (i + 1)) : (p[0] === 'attachments' ? 'Attachment ' : 'Authorization ') + (i + 1);
      return name + (p[2] ? ' · ' + words(p.slice(2).join(' ')) : '');
    }
    return words(p.join(' · ')).replace(/^./, function (c) { return c.toUpperCase(); });
  };

  // ---- page furniture
  CG.pageHead = function (o) {
    var left = CG.h('div', null,
      o.crumbs ? CG.h('nav', { class: 'crumbs', 'aria-label': 'Breadcrumb' }, o.crumbs.map(function (c, i) {
        return [i ? CG.h('span', { 'aria-hidden': 'true', text: '/' }) : null, c[1] ? CG.h('a', { href: c[1], text: c[0] }) : CG.h('span', { text: c[0] })];
      })) : null,
      CG.h('h1', { class: o.mono ? 'mono' : null, text: o.title }), o.chips ? CG.h('div', { class: 'chips' }, o.chips) : null,
      o.sub ? CG.h('p', { class: 'sub', text: o.sub }) : null);
    if (o.title) CG.setTitle(o.title);
    return CG.h('header', { class: 'pagehead' + (o.cls ? ' ' + o.cls : '') }, left, o.actions ? CG.h('div', { class: 'row' }, o.actions) : null);
  };
  CG.setTitle = function (t) { document.title = t ? t + ' — ClaimGuard Administration' : 'ClaimGuard — Administration'; };
  CG.empty = function (icon, title, text, action) {
    return CG.h('div', { class: 'card empty' }, CG.h('div', { class: 'ic-wrap' }, CG.icon(icon)), CG.h('h2', { text: title }), text ? CG.h('p', { class: 'muted', text: text }) : null, action || null);
  };
  CG.loading = function (text) { return CG.h('div', { class: 'loading', role: 'status' }, CG.h('span', { class: 'spinner', 'aria-hidden': 'true' }), CG.h('span', { text: text || 'Loading...' })); };
  CG.initials = function (name) { return String(name || '?').split(/\s+/).filter(Boolean).slice(-2).map(function (w) { return w.charAt(0).toUpperCase(); }).join('') || '?'; };

  // ---- personal templates: each role starts from its own preset, each person may change it (Customize page) -----------
  // Kept in this browser per badge. They change how the console looks and what is shown first, never what a person may do:
  // permissions come from the server and are checked again on every request, so hiding a menu item is the most a setting can do.
  // The clearance level itself is shown in a colour no setting can change (--role-c), so a person's level is always recognisable.
  CG.ROLES = {
    1: { key: 'viewer', label: 'Viewer', tagline: 'Read-only access to claims and findings', icon: 'eye', scope: 'Reads claims, findings and evidence. Decides nothing.' },
    2: { key: 'reviewer', label: 'Reviewer', tagline: 'Decide the findings routed to you', icon: 'check', scope: 'Decides standard findings routed to their inbox.' },
    3: { key: 'senior', label: 'Senior reviewer', tagline: 'High-severity decisions, countersign and tie-breaks', icon: 'star', scope: 'Decides high-severity findings; two seniors must agree.' },
    4: { key: 'admin', label: 'Supervisor', tagline: 'Queue, routing, payer rules, people and audit', icon: 'shield', scope: 'Runs the queue, rules, people and audit. Cannot decide claims.' }
  };
  CG.PERMS = {
    'claims.view': 'Read claims, findings and their evidence', 'claims.view_notes': 'Read the notes submitted with a claim',
    'claims.decide': 'Decide the findings routed to you', 'claims.decide_high': 'Decide high-severity findings and countersign',
    'claims.recheck': 'Send a corrected claim back through the engine', 'pii.unmask': 'Unmask identifiers for a stated, logged reason',
    'queue.view': 'See the queue and the system health', 'routing.manage': 'Change routing, shifts, intake and payer rules',
    'audit.view': 'Read the audit logs and claim traces', 'audit.verify': 'Verify the audit chain', 'users.manage': 'Create and manage accounts'
  };
  // Home widgets: which exist for each kind of Home, in their default order
  CG.WIDGETS = {
    work: [['tiles', 'Work counts'], ['rules', 'Open findings by rule'], ['keys', 'Keyboard shortcuts'], ['links', 'Shortcuts'], ['clearance', 'Your clearance']],
    viewer: [['clearance', 'Your clearance'], ['shortcuts', 'Quick filters'], ['recent', 'Recently flagged claims']],
    admin: [['alerts', 'Staffing alerts'], ['tiles', 'Queue counts'], ['fill', 'Inbox fill'], ['links', 'Admin tools'], ['clearance', 'Your clearance']]
  };
  CG.homeKind = function () { return CG.has('claims.decide') ? 'work' : CG.has('queue.view') ? 'admin' : 'viewer'; };
  CG.INBOX_COLS = [['lane', 'Lane'], ['issues', 'Issues'], ['todo', 'To decide'], ['priority', 'Priority'], ['stage', 'Stage'], ['lease', 'Lease']];
  var BASE = { theme: 'system', accent: 'brand', accentHex: '', density: 'comfortable', text: 'm', layout: 'side', home: '#/home', evidenceOpen: true, showAi: true,
    showPassed: false, showTodo: true, keyHints: false, inboxMode: 'claim', hiddenNav: [], sidebar: 'role', rail: false, corners: 'rounded', font: 'system',
    contrast: 'normal', motion: 'system', zebra: false, stickyHead: true, underline: false, idbar: true, greeting: true, homeWidgets: null, hiddenCols: [],
    focus: 'strong', vision: 'standard', shapes: false, spacing: 'normal', bigFocus: false, ruler: false, bg: 'none', bgDim: 'medium', glass: false, notify: true, notifyInbox: true, notifySound: 'chime', notifyVolume: 'medium', notifyDesktop: false };
  function preset(over) { var p = {}; Object.keys(BASE).forEach(function (k) { p[k] = BASE[k]; }); Object.keys(over).forEach(function (k) { p[k] = over[k]; }); return p; }
  CG.PRESETS = {
    1: preset({ showPassed: true }),
    2: preset({ keyHints: true }),
    3: preset({ keyHints: true }),
    4: preset({ density: 'compact', evidenceOpen: false, showPassed: true, inboxMode: 'rule', zebra: true })
  };
  // Ready-made looks anyone can apply on top of their role template
  CG.GALLERY = [
    ['Focus', 'Fewer distractions: no AI drafts, no greeting, evidence open, narrow menu.', { showAi: false, greeting: false, evidenceOpen: true, rail: true, idbar: false, density: 'comfortable' }],
    ['Night shift', 'Dark theme, slightly larger text, no animation.', { theme: 'dark', text: 'l', motion: 'reduce' }],
    ['High contrast', 'Stronger borders and text, underlined links, square corners.', { contrast: 'high', underline: true, corners: 'sharp', theme: 'light' }],
    ['Operator compact', 'Most rows per screen: compact, striped tables, sticky headers.', { density: 'compact', zebra: true, stickyHead: true, text: 's' }],
    ['Large and calm', 'Extra-large readable text, round corners, comfortable spacing.', { text: 'xl', font: 'humanist', corners: 'round', density: 'comfortable' }],
    ['Classic paper', 'Serif text on light paper with a slate accent.', { font: 'serif', theme: 'light', accent: 'slate', sidebar: 'light' }]
  ];
  // Background templates (pure CSS in style.css, "art-<name>") and the person's own photo
  CG.BACKGROUNDS = [['none', 'Plain'], ['aurora', 'Aurora'], ['neural', 'Neural grid'], ['dots', 'Dots'], ['constellation', 'Constellation'], ['dusk', 'Dusk'],
    ['ocean', 'Ocean'], ['forest', 'Forest'], ['paper', 'Paper'], ['photo', 'My photo']];
  var CHOICES = { theme: ['system', 'light', 'dark'], accent: ['brand', 'teal', 'blue', 'violet', 'amber', 'rose', 'slate', 'custom'], density: ['comfortable', 'compact'],
    text: ['s', 'm', 'l', 'xl'], layout: ['side', 'stacked'], inboxMode: ['claim', 'rule'], sidebar: ['role', 'dark', 'light', 'accent'], corners: ['sharp', 'rounded', 'round'],
    font: ['system', 'readable', 'humanist', 'serif', 'mono'], vision: ['standard', 'redgreen', 'blueyellow', 'mono'], spacing: ['normal', 'relaxed', 'wide'], contrast: ['normal', 'high'], motion: ['system', 'reduce'], focus: ['normal', 'strong', 'spotlight'],
    bg: ['none', 'aurora', 'neural', 'dots', 'constellation', 'dusk', 'ocean', 'forest', 'paper', 'photo'], bgDim: ['light', 'medium', 'strong'],
    notifySound: ['chime', 'ping', 'double', 'pulse', 'none'], notifyVolume: ['low', 'medium', 'high'] };
  var BOOLS = ['evidenceOpen', 'showAi', 'showPassed', 'showTodo', 'keyHints', 'rail', 'zebra', 'stickyHead', 'underline', 'idbar', 'greeting', 'glass', 'notify', 'notifyInbox', 'notifyDesktop', 'shapes', 'bigFocus', 'ruler'];
  var ALL_WIDGETS = ['clearance', 'tiles', 'rules', 'keys', 'links', 'shortcuts', 'recent', 'alerts', 'fill'];
  var COLS = CG.INBOX_COLS.map(function (c) { return c[0]; });
  function storeKey(what) { return 'cg.ui.v1.' + (what ? what + '.' : '') + (CG.state.me ? CG.state.me.badge_id : 'anon'); }
  function readJson(key, fallback) { try { return JSON.parse(localStorage.getItem(key) || 'null') || fallback; } catch (e) { return fallback; } }
  function writeJson(key, value) { try { localStorage.setItem(key, JSON.stringify(value)); return true; } catch (e) { return false; } }
  function readStored() { return CG.state.prefsFallback || readJson(storeKey(), {}); }
  function level() { return (CG.state.me && CG.PRESETS[CG.state.me.level]) ? CG.state.me.level : 2; }
  function uniq(list, allowed) { var seen = []; list.forEach(function (x) { if (typeof x === 'string' && allowed.test(x) && seen.indexOf(x) < 0) seen.push(x); }); return seen; }
  // Only known keys with allowed values survive, so an imported or tampered template cannot inject anything
  CG.cleanPrefs = function (raw) {
    var out = {};
    if (!raw || typeof raw !== 'object') return out;
    Object.keys(CHOICES).forEach(function (k) { if (CHOICES[k].indexOf(raw[k]) >= 0) out[k] = raw[k]; });
    BOOLS.forEach(function (k) { if (typeof raw[k] === 'boolean') out[k] = raw[k]; });
    if (typeof raw.accentHex === 'string' && /^#[0-9a-fA-F]{6}$/.test(raw.accentHex)) out.accentHex = raw.accentHex.toLowerCase();
    if (typeof raw.home === 'string' && /^#\/[a-z]+$/.test(raw.home)) out.home = raw.home;
    if (Array.isArray(raw.hiddenNav)) out.hiddenNav = uniq(raw.hiddenNav, /^#\/[a-z]+$/).slice(0, 30);
    if (Array.isArray(raw.homeWidgets)) out.homeWidgets = uniq(raw.homeWidgets, /^[a-z]+$/).filter(function (w) { return ALL_WIDGETS.indexOf(w) >= 0; });
    if (Array.isArray(raw.hiddenCols)) out.hiddenCols = uniq(raw.hiddenCols, /^[a-z]+$/).filter(function (c) { return COLS.indexOf(c) >= 0; });
    return out;
  };
  // only a base64 JPEG, PNG or WebP data URL under 4 MB: nothing that could close the url() and add CSS
  CG.photoOk = function (v) { return typeof v === 'string' && v.length < 4000000 && /^data:image\/(jpeg|png|webp);base64,[A-Za-z0-9+\/]+={0,2}$/.test(v); };
  // A picture file -> a downscaled JPEG data URL, read in the browser only (nothing is uploaded to the server). opts.square crops the
  // centre to a square of opts.max pixels (profile pictures); otherwise the longest side is limited to opts.max (backgrounds).
  CG.readImage = function (file, opts) {
    return new Promise(function (resolve, reject) {
      if (!file || !/^image\/(jpeg|png|webp|gif|bmp)$/.test(file.type)) { reject(new Error('Choose a JPEG, PNG, WebP, GIF or BMP picture.')); return; }
      if (file.size > 25 * 1024 * 1024) { reject(new Error('That picture is larger than 25 MB.')); return; }
      var url = URL.createObjectURL(file), img = new Image();
      img.onload = function () {
        URL.revokeObjectURL(url);
        var w = img.naturalWidth, hh = img.naturalHeight, sx = 0, sy = 0, sw = w, sh = hh, cw, ch;
        if (!w || !hh) { reject(new Error('That picture could not be read.')); return; }
        if (opts.square) { var side = Math.min(w, hh); sx = (w - side) / 2; sy = (hh - side) / 2; sw = sh = side; cw = ch = Math.min(opts.max, side); }
        else { var k = Math.min(1, opts.max / Math.max(w, hh)); cw = Math.round(w * k); ch = Math.round(hh * k); }
        var c = document.createElement('canvas'); c.width = cw; c.height = ch;
        var g = c.getContext('2d'); g.fillStyle = '#0b1426'; g.fillRect(0, 0, cw, ch); g.drawImage(img, sx, sy, sw, sh, 0, 0, cw, ch);
        var q = opts.quality || 0.82, out = c.toDataURL('image/jpeg', q);
        while (out.length > (opts.limit || 2500000) && q > 0.4) { q -= 0.12; out = c.toDataURL('image/jpeg', q); }
        if (!CG.photoOk(out)) { reject(new Error('That picture is too large even after shrinking it.')); return; }
        resolve(out);
      };
      img.onerror = function () { URL.revokeObjectURL(url); reject(new Error('That picture could not be read.')); };
      img.src = url;
    });
  };
  // The profile picture: per badge, in this browser only (others see your initials). Shown in the menu and on your sign-in card.
  function avatarKey(badge) { return 'cg.ui.v1.avatar.' + badge; }
  CG.avatarOf = function (badge) { var v = null; try { v = localStorage.getItem(avatarKey(badge)); } catch (e) { v = null; } v = (CG.state.avatarFallback || {})[badge] || v; return CG.photoOk(v) ? v : null; };
  CG.saveAvatar = function (badge, dataUrl) {
    if (!CG.photoOk(dataUrl)) throw new Error('That picture could not be used.');
    CG.state.avatarFallback = CG.state.avatarFallback || {}; delete CG.state.avatarFallback[badge];
    try { localStorage.setItem(avatarKey(badge), dataUrl); } catch (e) { CG.state.avatarFallback[badge] = dataUrl; }
  };
  CG.clearAvatar = function (badge) { if (CG.state.avatarFallback) delete CG.state.avatarFallback[badge]; try { localStorage.removeItem(avatarKey(badge)); } catch (e) { /* nothing stored */ } };
  CG.avatar = function (name, badge, extra) {
    var pic = badge ? CG.avatarOf(badge) : null;
    return CG.h('span', { class: 'avatar' + (pic ? ' pic' : '') + (extra ? ' ' + extra : ''), 'aria-hidden': 'true' }, pic ? CG.h('img', { src: pic, alt: '' }) : CG.initials(name));
  };
  CG.cleanName = function (s) { return String(s || '').replace(/[\u0000-\u001f\u007f<>]/g, '').trim().slice(0, 40); };
  // "#1d5fd1" -> a text colour that stays readable on it (WCAG relative luminance)
  CG.inkFor = function (hex) {
    var c = [1, 3, 5].map(function (i) { var v = parseInt(hex.substr(i, 2), 16) / 255; return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4); });
    var lum = 0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2];
    return (lum + 0.05) / 0.05 > 1.05 / (lum + 0.05) ? '#0b1218' : '#ffffff';
  };
  // Accessibility (Customize > Accessibility): one click sets the settings that help with a need; each can still be changed alone afterwards
  CG.A11Y_PRESETS = [
    ['Red-green colour blindness', 'Status colours that stay apart (orange, olive, blue) and a shape on every status.', { vision: 'redgreen', shapes: true }],
    ['Blue-yellow colour blindness', 'Red, magenta and teal status colours, and a shape on every status.', { vision: 'blueyellow', shapes: true }],
    ['No colour (greyscale)', 'Statuses told apart by shape, word and edge style only.', { vision: 'mono', shapes: true }],
    ['Dyslexia-friendly reading', 'An easy-to-read typeface, wider letter and line spacing, larger text and a reading ruler.', { font: 'readable', spacing: 'wide', text: 'l', ruler: true }],
    ['Low vision', 'Largest text, high contrast, a strong focus ring and underlined links.', { text: 'xl', contrast: 'high', bigFocus: true, underline: true, spacing: 'relaxed' }],
    ['Fewer distractions', 'No animation, a plain background and no dimming effects.', { motion: 'reduce', bg: 'none', glass: false, focus: 'strong' }]];
  CG.A11Y_KEYS = ['vision', 'shapes', 'font', 'spacing', 'text', 'ruler', 'contrast', 'bigFocus', 'underline', 'motion', 'bg', 'glass', 'focus'];

  // reading ruler: a soft band that follows the pointer so the eye keeps its line; its position goes through the CSS object model
  var ruler = null;
  function moveRuler(e) { document.documentElement.style.setProperty('--ruler-y', e.clientY + 'px'); }
  function setRuler(on) {
    if (on && !ruler && document.body) {
      ruler = CG.h('div', { class: 'ruler', 'aria-hidden': 'true' }); document.body.appendChild(ruler);
      document.addEventListener('mousemove', moveRuler, { passive: true });
    } else if (!on && ruler) {
      ruler.remove(); ruler = null; document.removeEventListener('mousemove', moveRuler);
    }
  }

  CG.prefs = {
    preset: function () { return CG.PRESETS[level()]; },
    all: function () {
      var p = {}, base = CG.prefs.preset(), mine = CG.cleanPrefs(readStored());
      Object.keys(base).forEach(function (k) { p[k] = k in mine ? mine[k] : base[k]; });
      if (!p.homeWidgets) p.homeWidgets = CG.WIDGETS[CG.state.me ? CG.homeKind() : 'work'].map(function (w) { return w[0]; });
      return p;
    },
    get: function (k) { return CG.prefs.all()[k]; },
    custom: function () { return CG.cleanPrefs(readStored()); },
    save: function (changes) {
      var mine = CG.cleanPrefs(readStored()), base = CG.prefs.preset(), clean = CG.cleanPrefs(changes);
      Object.keys(clean).forEach(function (k) { mine[k] = clean[k]; if (JSON.stringify(mine[k]) === JSON.stringify(base[k])) delete mine[k]; });
      if (!writeJson(storeKey(), mine)) CG.state.prefsFallback = mine;      // private window: the change lasts for this page only
      CG.prefs.apply();
    },
    replace: function (prefs) { var clean = CG.cleanPrefs(prefs); if (!writeJson(storeKey(), clean)) CG.state.prefsFallback = clean; CG.prefs.apply(); },
    reset: function () { CG.state.prefsFallback = null; try { localStorage.removeItem(storeKey()); } catch (e) { /* nothing stored */ } CG.prefs.apply(); },
    exportText: function () { return JSON.stringify({ claimguard_ui_template: 2, role: CG.ROLES[level()].key, prefs: CG.prefs.custom() }, null, 2); },
    importText: function (text) {
      var data = JSON.parse(text), p = CG.cleanPrefs(data && data.prefs ? data.prefs : data);
      if (!Object.keys(p).length) throw new Error('No recognised settings in that template.');
      CG.prefs.save(p); return Object.keys(p).length;
    },
    // named profiles: several saved looks per person ("Morning", "Long shift"), switched in one click
    profiles: function () {
      var raw = readJson(storeKey('profiles'), {}), out = {};
      Object.keys(raw).slice(0, 12).forEach(function (n) { var c = CG.cleanName(n); if (c) out[c] = CG.cleanPrefs(raw[n]); });
      return out;
    },
    saveProfile: function (name) {
      var n = CG.cleanName(name), all = CG.prefs.profiles();
      if (!n) throw new Error('Give the profile a name.');
      if (!(n in all) && Object.keys(all).length >= 12) throw new Error('Twelve profiles at most. Delete one first.');
      all[n] = CG.prefs.custom(); writeJson(storeKey('profiles'), all); return n;
    },
    useProfile: function (name) { var all = CG.prefs.profiles(); if (all[name]) CG.prefs.replace(all[name]); },
    deleteProfile: function (name) { var all = CG.prefs.profiles(); delete all[name]; writeJson(storeKey('profiles'), all); },
    // the background photo: a downscaled JPEG data URL kept apart from the template (too big to share, and never exported)
    photo: function () { var v = null; try { v = localStorage.getItem(storeKey('bg')); } catch (e) { v = null; } v = CG.state.photoFallback || v || null; return CG.photoOk(v) ? v : null; },
    savePhoto: function (dataUrl) {
      if (!CG.photoOk(dataUrl)) throw new Error('That picture could not be used.');
      CG.state.photoFallback = null;
      try { localStorage.setItem(storeKey('bg'), dataUrl); } catch (e) { CG.state.photoFallback = dataUrl; }   // full or private storage: this page only
      CG.prefs.save({ bg: 'photo' });
    },
    clearPhoto: function () { CG.state.photoFallback = null; try { localStorage.removeItem(storeKey('bg')); } catch (e) { /* nothing stored */ } if (CG.prefs.get('bg') === 'photo') CG.prefs.save({ bg: 'none' }); else CG.prefs.apply(); },
    apply: function () {
      var p = CG.prefs.all(), root = document.documentElement;
      if (p.theme === 'system') root.removeAttribute('data-theme'); else root.setAttribute('data-theme', p.theme);
      ['accent', 'density', 'text', 'layout', 'sidebar', 'corners', 'font', 'contrast', 'motion', 'focus', 'vision', 'spacing'].forEach(function (k) { root.setAttribute('data-' + k, p[k]); });
      root.setAttribute('data-rail', p.rail ? 'on' : 'off'); root.setAttribute('data-zebra', p.zebra ? 'on' : 'off');
      root.setAttribute('data-sticky', p.stickyHead ? 'on' : 'off'); root.setAttribute('data-underline', p.underline ? 'on' : 'off');
      root.setAttribute('data-shapes', p.shapes || p.vision !== 'standard' ? 'on' : 'off'); root.setAttribute('data-bigfocus', p.bigFocus ? 'on' : 'off');
      setRuler(!!(p.ruler && CG.state.me));
      // the background: a template is pure CSS; a photo goes through the CSS object model as a checked data: URL (img-src allows data:)
      var photo = p.bg === 'photo' && CG.state.me ? CG.prefs.photo() : null;
      root.setAttribute('data-bg', CG.state.me && (p.bg !== 'photo' || photo) ? p.bg : 'none'); root.setAttribute('data-bgdim', p.bgDim); root.setAttribute('data-glass', p.glass ? 'on' : 'off');
      if (photo) root.style.setProperty('--bg-photo', 'url("' + photo + '")'); else root.style.removeProperty('--bg-photo');
      root.setAttribute('data-role', CG.state.me ? CG.ROLES[level()].key : 'none');
      // a custom accent is the only value written through the CSS object model (allowed by the CSP, unlike inline style attributes);
      // the stylesheet derives the hover, soft and line shades from it with color-mix
      if (p.accent === 'custom' && p.accentHex) { root.style.setProperty('--accent', p.accentHex); root.style.setProperty('--accent-ink', CG.inkFor(p.accentHex)); }
      else { root.style.removeProperty('--accent'); root.style.removeProperty('--accent-ink'); }
    }
  };

  // ---- clearance: the level badge shown next to every person, in the role's fixed colour
  CG.levelBadge = function (lvl, opts) {
    var r = CG.ROLES[lvl] || CG.ROLES[1], pips = CG.h('span', { class: 'pips', 'aria-hidden': 'true' });
    for (var i = 1; i <= 4; i++) pips.appendChild(CG.h('i', { class: i <= lvl ? 'on' : '' }));
    return CG.h('span', { class: 'lvl lvl-' + r.key + (opts && opts.large ? ' large' : ''), title: 'Clearance level ' + lvl + ' of 4: ' + r.label },
      CG.h('b', { text: 'L' + lvl }), CG.h('span', { class: 'lvl-name', text: r.label }), pips, CG.h('span', { class: 'sr-only', text: 'clearance level ' + lvl + ' of 4' }));
  };

  CG.status = function (s) { return CG.h('span', { class: 'dot ' + s, role: 'img', 'aria-label': s }); };
  CG.when = function (iso) { var d = new Date(iso); return isNaN(d) ? String(iso || '-') : d.toLocaleString(); };

  // ---- notifications: the bell, the pop-ups (5 seconds each), and the detail window ----------------------------------------------------
  // The server keeps each person's notifications (claim arrived, information requested by the pipeline, lease expired). The live stream only
  // says "changed" (topic: notifications); this code then reads GET /work/notifications. The stream is opened once after sign-in and is not
  // one of the per-page streams, so moving between pages does not close it. A notification pops up once; a reload lists the old ones in the
  // bell without popping them again. What happened while nobody was signed in waits in the bell too: the first read after signing in only
  // marks what is there as seen, and every later read pops what is new (no clock comparison, so a browser whose clock is off still pops).
  // Everything the server sends is put on the page as text only.
  var POPUP_MS = 5000, MAX_POPUPS = 4;
  var KINDS = { claim_arrived: ['New claim', 'info'], info_requested_auto: ['Pipeline action', 'ai'], lease_expired: ['Returned to pool', 'B'] };
  var N = CG.notes = { items: [], seen: null, es: null, timer: null, badge: null, bell: null, count: null, panel: null, pops: null, detail: null, started: false, primed: false, gen: 0 };

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
    N.badge = me.badge_id; N.started = true; N.seen = null; N.items = []; N.primed = false; N.gen++;
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
    N.started = false; N.items = []; N.seen = null; N.badge = null; N.primed = false; N.gen++;
    N.hidePanel(); N.closeDetail();
    if (N.pops) CG.clear(N.pops);
    N.paint();
  };

  N.refresh = function () {
    var gen = N.gen;
    return CG.api('GET', '/work/notifications').then(function (r) {
      if (gen !== N.gen || !N.started) return;            // signed out (or someone else signed in) while this read was on its way
      var rows = (r && r.notifications) || [];
      if (N.seen === null) N.seen = loadSeen();
      var fresh = [];
      rows.forEach(function (n) {
        if (N.seen[n.id]) return;
        N.seen[n.id] = true;
        if (N.primed) fresh.push(n);                       // the first read after signing in only takes stock; older ones wait in the bell
      });
      N.primed = true;
      N.items = rows;
      saveSeen();
      fresh.reverse().forEach(N.popup);
      N.paint();
      if (N.panel) N.paintPanel();
    }, function (e) { if (gen === N.gen && e && e.status === 404) { N.stop(); if (N.bell) N.bell.hidden = true; } });      // no notifications on this server
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
