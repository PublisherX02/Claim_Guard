/* ClaimGuard console: the administrator's data-center board. One read of /queue/stats a minute, shown in five tabs (overview, live flow,
 * rules, operators, reports). The numbers are counts only: no claim content and no patient data reach this page. The daily report is the
 * server's own file (/queue/report), so what is downloaded is what the server wrote and audited, not something this page assembled. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  CG.views = CG.views || {};

  var REFRESH_MS = 60000;
  var TABS = [['overview', 'Overview'], ['flow', 'Live flow'], ['rules', 'Rules'], ['operators', 'Operators'], ['reports', 'Reports']];

  function localMidnight() { var d = new Date(); d.setHours(0, 0, 0, 0); return Math.floor(d.getTime() / 1000); }
  function clock(sec) { return sec ? new Date(sec * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }) : '-'; }
  function pct(a, b) { return b > 0 ? Math.round(1000 * a / b) / 10 + '%' : '-'; }
  function tile(n, label, note) { return h('div', { class: 'tile' }, h('div', { class: 'n', text: String(n) }), h('div', { class: 'l', text: label }), note ? h('div', { class: 'muted small', text: note }) : null); }
  function ruleName(id) { return CG.RULES && CG.RULES[id] ? id + ' ' + CG.RULES[id] : id; }

  /* A line chart of hourly points. series = [{ name, key, cls }]; every point carries a tooltip and the same numbers are given as a table below. */
  function lineChart(points, series, title) {
    var W = 640, H = 190, L = 34, B = 20, T = 8, R = 10, n = points.length;
    var max = Math.max.apply(null, [1].concat(points.map(function (p) { return Math.max.apply(null, series.map(function (s) { return p[s.key] || 0; })); })));
    var svg = CG.svg('svg', { viewBox: '0 0 ' + W + ' ' + H, class: 'chart', role: 'img', 'aria-label': title + '. Highest point: ' + max + '.', preserveAspectRatio: 'none' });
    function x(i) { return L + i * (W - L - R) / Math.max(1, n - 1); }
    function y(v) { return T + (H - T - B) * (1 - v / max); }
    [0, 0.5, 1].forEach(function (f) {
      svg.appendChild(CG.svg('line', { x1: L, x2: W - R, y1: y(max * f), y2: y(max * f), class: 'grid' }));
      svg.appendChild(CG.svgText({ x: L - 4, y: y(max * f) + 3, class: 'axis', 'text-anchor': 'end' }, String(Math.round(max * f))));
    });
    series.forEach(function (s) {
      svg.appendChild(CG.svg('polyline', { points: points.map(function (p, i) { return x(i).toFixed(1) + ',' + y(p[s.key] || 0).toFixed(1); }).join(' '), class: 'line-' + s.cls, fill: 'none' }));
      points.forEach(function (p, i) {
        var t = CG.svg('title'); t.textContent = clock(p.hour_start) + ': ' + (p[s.key] || 0) + ' ' + s.name;
        svg.appendChild(CG.svg('circle', { cx: x(i), cy: y(p[s.key] || 0), r: 2.6, class: 'dot-' + s.cls }, t));
      });
    });
    [0, Math.floor((n - 1) / 2), n - 1].forEach(function (i) {
      if (i >= 0 && i < n) svg.appendChild(CG.svgText({ x: x(i), y: H - 5, class: 'axis', 'text-anchor': i === 0 ? 'start' : i === n - 1 ? 'end' : 'middle' }, clock(points[i].hour_start)));
    });
    return h('figure', { class: 'figure' }, svg, h('div', { class: 'legend' }, series.map(function (s) { return h('span', null, h('i', { class: 'swatch fill-' + s.cls }), ' ' + s.name); })));
  }

  CG.views.board = function (root) {
    var tab = CG.state.boardTab || 'overview', data = null, shortages = [], stamp = h('span', { class: 'muted small', role: 'status' });
    var tabbar = h('div', { class: 'tabs', role: 'tablist', 'aria-label': 'Data center sections' });
    var panel = h('div', { class: 'stack', role: 'tabpanel', id: 'board-panel', tabindex: '-1' });
    var buttons = {};

    TABS.forEach(function (t) {
      var b = h('button', { class: 'tab', type: 'button', role: 'tab', id: 'tab-' + t[0], 'aria-controls': 'board-panel', text: t[1], onclick: function () { select(t[0]); } });
      b.addEventListener('keydown', function (e) {
        var i = TABS.map(function (x) { return x[0]; }).indexOf(t[0]), j = e.key === 'ArrowRight' ? i + 1 : e.key === 'ArrowLeft' ? i - 1 : -1;
        if (j < 0) return;
        e.preventDefault(); var next = TABS[(j + TABS.length) % TABS.length][0]; select(next); buttons[next].focus();
      });
      buttons[t[0]] = b; tabbar.appendChild(b);
    });

    root.appendChild(h('div', { class: 'row between' },
      h('div', null, h('h1', { text: 'Data center' }), h('div', { class: 'row tight' }, h('span', { class: 'dot ok', 'aria-hidden': 'true' }), h('span', { class: 'muted small', text: 'Live: updates within seconds of a change, and every minute.' }), stamp)),
      h('button', { class: 'btn', type: 'button', text: 'Refresh now', onclick: load })));
    root.appendChild(tabbar);
    root.appendChild(panel);

    function select(name) {
      tab = name; CG.state.boardTab = name;
      TABS.forEach(function (t) { var on = t[0] === name; buttons[t[0]].setAttribute('aria-selected', String(on)); buttons[t[0]].tabIndex = on ? 0 : -1; buttons[t[0]].className = 'tab' + (on ? ' on' : ''); });
      panel.setAttribute('aria-labelledby', 'tab-' + name);
      draw();
    }

    function draw() {
      CG.clear(panel);
      if (!data) { panel.appendChild(h('p', { class: 'muted', text: 'Loading the numbers...' })); return; }
      ({ overview: overview, flow: flow, rules: rules, operators: operators, reports: reports })[tab]();
    }

    function overview() {
      var c = data.claims, v = data.rule_violations, o = data.operators;
      if (shortages.length) panel.appendChild(h('div', { class: 'notice bad' }, h('b', { text: 'Needs attention: ' }), shortages.join('; ')));
      panel.appendChild(h('div', { class: 'tiles' },
        tile(c.received, 'claims received today'), tile(c.processed, 'claims processed today', pct(c.processed, c.received) + ' of those received'),
        tile(c.unprocessed, 'still unprocessed', 'the whole backlog, not only today'), tile((c.backlog_by_state || {}).awaiting_countersign || 0, 'awaiting countersign', 'one senior has signed; a second must agree'), tile(v.total, 'rule violations today', v.rules_violated + ' different rules'),
        tile(o.worked, 'operators worked today', o.on_shift + ' on shift now'), tile(c.processed_by_system, 'cleared by the system', c.processed_by_person + ' decided by people')));
      var order = Object.keys(c.backlog_by_state);
      panel.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Where the unprocessed claims are' }),
        c.unprocessed ? CG.hbars(order.filter(function (k) { return c.backlog_by_state[k] > 0; }).map(function (k) { return { label: k, value: c.backlog_by_state[k], cls: k === 'leased' ? 's0' : 's1' }; }), c.unprocessed)
          : h('p', { class: 'muted', text: 'Nothing is waiting.' })));
      panel.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Last 24 hours' }), lineChart(data.hourly, [{ name: 'received', key: 'received', cls: 's0' }, { name: 'processed', key: 'processed', cls: 's3' }], 'Claims received and processed per hour')));
    }

    function flow() {
      var hr = data.hourly;
      panel.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Claims received against processed, and rules violated' }),
        h('p', { class: 'muted', text: 'One point per hour for the last 24 hours. Violations are the failed rule results on the claims received in that hour.' }),
        lineChart(hr, [{ name: 'received', key: 'received', cls: 's0' }, { name: 'processed', key: 'processed', cls: 's3' }, { name: 'rule violations', key: 'violations', cls: 's2' }], 'Claims received, processed and rule violations per hour'),
        h('details', null, h('summary', { text: 'Show the same numbers as a table' }),
          CG.table([{ label: 'Hour', get: function (p) { return clock(p.hour_start); } }, { label: 'Received', num: true, get: function (p) { return p.received; } }, { label: 'Processed', num: true, get: function (p) { return p.processed; } }, { label: 'Violations', num: true, get: function (p) { return p.violations; } }], hr.slice().reverse()))));
    }

    function rules() {
      var list = data.rule_violations.by_rule;
      panel.appendChild(h('div', { class: 'card' }, h('h2', { text: 'How often each rule was violated today' }),
        list.length ? CG.hbars(list.filter(function (r) { return r.violated > 0; }).map(function (r) { return { label: ruleName(r.rule_id), value: r.violated, cls: 's2' }; }), data.rule_violations.total)
          : h('p', { class: 'muted', text: 'No rule results yet today.' }),
        list.length ? CG.table([{ label: 'Rule', get: function (r) { return h('span', null, h('b', { class: 'mono', text: r.rule_id }), ' ', CG.RULES && CG.RULES[r.rule_id] ? CG.RULES[r.rule_id] : ''); } },
          { label: 'Violated', num: true, get: function (r) { return r.violated; } }, { label: 'Could not assess', num: true, get: function (r) { return r.unable_to_assess; } }], list) : null));
    }

    function operators() {
      var list = data.operators.list, top = Math.max.apply(null, [1].concat(list.map(function (o) { return o.claims_processed + o.processing_now + o.waiting_in_inbox; })));
      panel.appendChild(h('div', { class: 'tiles' }, tile(data.operators.worked, 'operators worked today'), tile(data.operators.on_shift, 'on shift now'), tile(data.operators.claims_processed_by_operators, 'claims they processed')));
      panel.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Each operator right now' }),
        list.length ? CG.table([
          { label: 'Operator', get: function (o) { return h('span', null, h('b', { class: 'mono', text: o.badge_id }), o.on_shift ? '' : h('span', { class: 'muted small', text: ' off shift' })); } },
          { label: 'Processing now', num: true, get: function (o) { return o.processing_now; } },
          { label: 'Waiting in inbox', num: true, get: function (o) { return o.waiting_in_inbox; } },
          { label: 'Processed today', num: true, get: function (o) { return o.claims_processed; } },
          { label: 'Load', get: function (o) {
            var bar = h('div', { class: 'bar multi', title: 'processed, processing, waiting' });
            [['s3', o.claims_processed], ['s0', o.processing_now], ['s1', o.waiting_in_inbox]].forEach(function (p) { var i = h('i', { class: 'bar-' + p[0] }); i.style.width = (100 * p[1] / top) + '%'; bar.appendChild(i); });
            return bar; } }], list) : h('p', { class: 'muted', text: 'Nobody is on shift and nobody has decided a claim today.' })));
      panel.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Productivity today' }),
        h('p', { class: 'muted', text: 'Claims per hour is the claims an operator decided, divided by the hours between their first and last decision (at least one).' }),
        list.some(function (o) { return o.decisions > 0; }) ? CG.table([
          { label: 'Operator', get: function (o) { return h('b', { class: 'mono', text: o.badge_id }); } },
          { label: 'Claims', num: true, get: function (o) { return o.claims_processed; } }, { label: 'Decisions', num: true, get: function (o) { return o.decisions; } },
          { label: 'Confirmed', num: true, get: function (o) { return o.confirmed; } }, { label: 'Dismissed', num: true, get: function (o) { return o.dismissed; } },
          { label: 'Info requested', num: true, get: function (o) { return o.information_requested; } },
          { label: 'First', num: true, get: function (o) { return clock(o.first_decision_at); } }, { label: 'Last', num: true, get: function (o) { return clock(o.last_decision_at); } },
          { label: 'Claims per hour', num: true, get: function (o) { return o.claims_per_hour; } }], list.filter(function (o) { return o.decisions > 0; })) : h('p', { class: 'muted', text: 'No decisions yet today.' })));
    }

    function reports() {
      var href = '/api/v1/queue/report?since=' + localMidnight();
      var link = h('a', { class: 'btn primary', href: href, download: 'claimguard-report.json' }, CG.icon('upload'), "Download today's report (JSON)");
      panel.appendChild(h('div', { class: 'card stack' }, h('h2', { text: "Today's report" }),
        h('p', { class: 'muted', text: 'Claims received, processed and still unprocessed; how often each rule was violated; how many operators worked and what each one processed, with a productivity figure. Counts only. Each download is written to the audit trail.' }),
        h('div', { class: 'row' }, link),
        h('h3', { text: 'What the file holds right now' }),
        h('pre', { class: 'jsonview', tabindex: '0', 'aria-label': 'Preview of the report', text: JSON.stringify({ claims: data.claims, rule_violations: { total: data.rule_violations.total, by_rule: data.rule_violations.by_rule }, operators: { worked: data.operators.worked, list: data.operators.list.map(function (o) { return { badge_id: o.badge_id, claims_processed: o.claims_processed, claims_per_hour: o.claims_per_hour }; }) } }, null, 2) })));
    }

    function load() {
      Promise.all([CG.api('GET', '/queue/stats?since=' + localMidnight()), CG.api('GET', '/queue/dashboard')]).then(function (res) {
        data = res[0]; shortages = res[1].shortages || [];
        stamp.textContent = 'Updated ' + new Date().toLocaleTimeString();
        draw();
      }, function (e) { stamp.textContent = 'Could not refresh: ' + e.message; });
    }

    select(tab);
    load();
    CG.every(REFRESH_MS, load);
    CG.live('queue', load);   // a claim moving between states is shown within seconds, not at the next minute
  };
})();
