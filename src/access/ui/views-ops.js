/* ClaimGuard console: the system health panel, the audit panel and the claim trace (administrators).
 * Same rule as every file here: server text only ever becomes text, never markup. Charts are SVG built with createElementNS. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  CG.views = CG.views || {};

  var STATUS_TEXT = { ok: 'Healthy', degraded: 'Degraded', down: 'Down', unknown: 'Not reported yet' };
  var STATUS_NOTICE = { ok: 'good', degraded: 'warn', down: 'bad', unknown: 'info' };
  var EDGES = [5, 10, 25, 50, 100, 250, 500, 1000, 2500];

  function dur(secs) { return secs === null || secs === undefined ? '-' : CG.fmtDuration(secs); }
  function ms(v) { return v === null || v === undefined ? 'over 2.5 s' : v + ' ms'; }
  function tile(n, label) { return h('div', { class: 'tile' }, h('div', { class: 'n', text: String(n) }), h('div', { class: 'l', text: label })); }
  function clock(epochSeconds) { return new Date(epochSeconds * 1000).toLocaleTimeString(); }

  // ---- health -----------------------------------------------------------------------------------------------------------
  function componentCard(c) {
    var extra = null;
    if (c.id === 'scheduler' && c.jobs) {
      extra = CG.table([
        { label: 'Job', get: function (j) { return j.name; } },
        { label: 'Status', get: function (j) { return h('span', null, CG.status(j.status), ' ' + STATUS_TEXT[j.status]); } },
        { label: 'Every', num: true, get: function (j) { return j.every_seconds + ' s'; } },
        { label: 'Last run', get: function (j) { return j.last_at ? CG.fmtTime(j.last_at) : '-'; } },
        { label: 'Runs / failures', num: true, get: function (j) { return j.runs === undefined ? '-' : j.runs + ' / ' + j.failures; } }
      ], c.jobs);
    }
    return h('div', { class: 'card comp ' + c.status + (c.id === 'scheduler' ? ' wide' : '') },
      h('div', { class: 'row between' }, h('b', null, CG.status(c.status), ' ' + c.label), h('span', { class: 'muted small', text: STATUS_TEXT[c.status] + (c.weight === 'optional' ? ' - optional' : '') })),
      h('p', { class: 'small', text: c.detail }),
      h('div', { class: 'muted small', text: 'checked ' + clock(c.checked_at) + ', took ' + c.latency_ms + ' ms' }),
      extra);
  }

  CG.views.health = function (root) {
    var body = h('div'), stamp = h('span', { class: 'muted small' }), busy = false;
    function render(r) {
      CG.clear(body);
      var m = r.metrics, by = {};
      r.components.forEach(function (c) { by[c.id] = c; });
      var broken = r.components.filter(function (c) { return c.status === 'down' || c.status === 'degraded'; });
      body.appendChild(h('div', { class: 'notice ' + STATUS_NOTICE[r.overall] },
        h('b', { text: r.overall === 'ok' ? 'Everything is working.' : r.overall === 'down' ? 'Something essential is down.' : 'Working, with problems.' }),
        broken.length ? h('ul', { class: 'plain' }, broken.map(function (c) { return h('li', null, h('b', { text: c.label + ': ' }), c.detail); })) : ' All checks passed.'));
      var errPct = m.requests ? (100 * (m.server_errors) / m.requests).toFixed(1) + '%' : '0%';
      body.appendChild(h('div', { class: 'tiles' }, tile(m.requests, 'requests, last ' + m.window_minutes + ' min'), tile(errPct, 'server errors'), tile(m.client_errors, 'refused or invalid requests'),
        tile(ms(m.p50_ms), 'median response (upper bound)'), tile(ms(m.p95_ms), '95% faster than'), tile(ms(m.p99_ms), '99% faster than'), tile(dur(m.uptime_seconds), 'this API process up for')));

      body.appendChild(h('h2', { text: 'Services', class: 'sect' }));
      ['core', 'optional'].forEach(function (weight) {
        var list = r.components.filter(function (c) { return c.weight === weight; });
        if (!list.length) return;
        body.appendChild(h('h3', { class: 'muted small caps', text: weight === 'core' ? 'The system cannot work without these' : 'Helpers: claims are still reviewed if these are down' }));
        body.appendChild(h('div', { class: 'compgrid' }, list.map(componentCard)));
      });

      var flow = by.flow && by.flow.waiting ? by.flow : null;
      if (flow) {
        body.appendChild(h('h2', { text: 'Claim flow', class: 'sect' }));
        var counts = flow.counts || {}, keys = Object.keys(counts).sort();
        body.appendChild(h('div', { class: 'tiles' }, tile(flow.waiting.decide, 'waiting for a reviewer'), tile(flow.waiting.decide_high, 'waiting for a senior'),
          tile(dur(flow.oldest_waiting_seconds.decide), 'oldest standard wait'), tile(dur(flow.oldest_waiting_seconds.decide_high), 'oldest senior wait'),
          tile(flow.dead_letters, 'in the dead-letter list'), tile(flow.lapsed_leases, 'lapsed leases not returned'), tile(by.outbox ? by.outbox.pending : 0, 'waiting to be published')));
        if (keys.length) body.appendChild(h('details', { class: 'card' }, h('summary', { text: 'All claim states (' + keys.length + ')' }),
          CG.table([{ label: 'State', get: function (k) { return k.replace(/\|/g, ' - ').replace(/_/g, ' '); } }, { label: 'Claims', num: true, get: function (k) { return counts[k]; } }], keys)));
      }

      body.appendChild(h('h2', { text: 'Traffic to this API (last ' + m.window_minutes + ' minutes)', class: 'sect' }));
      var labels = m.series.map(function (p) { return new Date(p.t * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }); });
      body.appendChild(h('div', { class: 'card' },
        CG.barChart({ title: 'Requests per minute', labels: labels, height: 150, series: [
          { name: 'succeeded', cls: 's0', values: m.series.map(function (p) { return p.n - p.e4 - p.e5; }) },
          { name: 'refused or invalid (4xx)', cls: 's1', values: m.series.map(function (p) { return p.e4; }) },
          { name: 'server errors (5xx)', cls: 's2', values: m.series.map(function (p) { return p.e5; }) }] }),
        h('p', { class: 'muted small', text: 'One bar per minute. This is this API process since it started; it does not show other servers.' })));
      var total = m.latency_counts.reduce(function (a, b) { return a + b; }, 0);
      body.appendChild(h('div', { class: 'card' }, h('h3', { text: 'How fast requests were answered' }),
        total ? CG.hbars(m.latency_counts.map(function (c, i) { return { label: i < EDGES.length ? 'up to ' + EDGES[i] + ' ms' : 'slower than 2.5 s', value: c, cls: i >= 6 ? 's1' : 's0' }; }), total)
          : h('p', { class: 'muted', text: 'No requests yet in this window.' })));
      if (m.routes.length) body.appendChild(h('details', { class: 'card' }, h('summary', { text: 'Busiest routes (' + m.routes.length + ')' }), CG.table([
        { label: 'Route', get: function (x) { return h('span', { class: 'mono', text: x.route }); } },
        { label: 'Requests', num: true, get: function (x) { return x.requests; } },
        { label: '4xx', num: true, get: function (x) { return x.client_errors; } },
        { label: '5xx', num: true, get: function (x) { return x.server_errors; } },
        { label: 'Mean', num: true, get: function (x) { return x.mean_ms === null ? '-' : x.mean_ms + ' ms'; } },
        { label: '95% under', num: true, get: function (x) { return ms(x.p95_ms); } }], m.routes)));
      if (m.recent_errors.length) body.appendChild(h('div', { class: 'card' }, h('h3', { text: 'Recent server errors' }), CG.table([
        { label: 'When', get: function (e) { return CG.fmtTime(e.at); } }, { label: 'Route', get: function (e) { return h('span', { class: 'mono', text: e.route }); } },
        { label: 'Status', num: true, get: function (e) { return e.status; } }], m.recent_errors)));
      stamp.textContent = 'Updated ' + new Date().toLocaleTimeString() + '. Refreshes every 15 seconds.';
    }
    function load() {
      if (busy) return;
      busy = true;
      CG.api('GET', '/ops/health').then(render, CG.fail).then(function () { busy = false; }, function () { busy = false; });
    }
    root.appendChild(h('div', { class: 'row between' }, h('h1', { text: 'System health' }), h('div', { class: 'row' }, stamp, h('button', { class: 'btn', type: 'button', text: 'Check now', onclick: load }))));
    root.appendChild(body);
    load();
    CG.every(15000, load);
  };

  // ---- audit ------------------------------------------------------------------------------------------------------------
  function describe(e) {
    var who = e.actor || e.badge_id || '', t = e.event_type, c = e.claim_id;
    switch (t) {
      case 'login_success': return who + ' signed in';
      case 'login_failure': return 'failed sign-in (' + (e.reason || 'unknown') + ')';
      case 'lockout': return who + ' was locked out';
      case 'logout': return who + ' signed out';
      case 'forbidden': return who + ' was refused ' + e.method + ' ' + e.path;
      case 'decision': return who + ' ' + String(e.action).replace(/_/g, ' ') + ' on ' + e.rule_id + ' of ' + c;
      case 'unmask': return who + ' revealed identifiers of ' + c + ' (' + e.reason + ')';
      case 'user_created': return e.actor + ' created user ' + e.badge_id + ' at level ' + e.level;
      case 'user_updated': return e.actor + ' changed ' + e.badge_id + ': ' + (Array.isArray(e.changes) ? e.changes.join(', ') : e.changes);
      case 'claim_dealt': return c + ' was dealt to ' + e.badge_id;
      case 'claim_decided_green': return who + ' ' + String(e.action).replace(/_/g, ' ') + ' on clean claim ' + c;
      case 'claim_signoff': return who + ' signed ' + c + ' (' + e.stage + ': ' + e.outcome + ')';
      case 'lease_expired': return 'lease on ' + c + ' ran out for ' + e.badge_id;
      case 'triage_receipt': return c + ' triaged: lane ' + e.lane + ', score ' + e.score;
      case 'routing_config_changed': return e.actor + ' changed routing settings to version ' + e.version;
      default: return t.replace(/_/g, ' ') + (who ? ' by ' + who : '') + (c ? ' on ' + c : '');
    }
  }
  var PERIODS = [['1 hour', 3600], ['24 hours', 86400], ['7 days', 604800], ['all time', 0]];
  function csvCell(v) { var s = v === null || v === undefined ? '' : String(v); if (/^[=+\-@\t\r]/.test(s)) s = "'" + s; return '"' + s.replace(/"/g, '""') + '"'; }

  CG.views.audit = function (root) {
    var offset = 0, limit = 50, last = { rows: [], matched: 0 };
    var summaryBox = h('div'), table = h('div'), pager = h('div', { class: 'row' }), verdict = h('div');
    var type = h('select', { 'aria-label': 'Event type' }, h('option', { value: '', text: 'Any event' }));
    var badge = h('input', { type: 'text', placeholder: 'Badge, e.g. CG-2002', maxlength: '64', 'aria-label': 'Badge' });
    var claim = h('input', { type: 'text', placeholder: 'Claim id', maxlength: '80', 'aria-label': 'Claim id' });
    var period = h('select', { 'aria-label': 'Period' }, PERIODS.map(function (p, i) { return h('option', { value: String(p[1]), text: p[0], selected: i === 3 }); }));

    function query(extra) {
      var q = '?newest_first=true&limit=' + (extra && extra.limit || limit) + '&offset=' + (extra && extra.offset !== undefined ? extra.offset : offset);
      if (type.value) q += '&event_type=' + encodeURIComponent(type.value);
      if (badge.value.trim()) q += '&badge=' + encodeURIComponent(badge.value.trim());
      if (claim.value.trim()) q += '&claim_id=' + encodeURIComponent(claim.value.trim());
      if (Number(period.value)) q += '&since=' + (Math.floor(Date.now() / 1000) - Number(period.value));
      return q;
    }
    function loadSummary() {
      CG.api('GET', '/ops/audit/summary?hours=24').then(function (s) {
        CG.clear(summaryBox);
        Object.keys(s.by_type).sort().forEach(function (k) { type.appendChild(h('option', { value: k, text: k.replace(/_/g, ' ') + ' (' + s.by_type[k] + ')' })); });
        var f = s.failures;
        summaryBox.appendChild(h('div', { class: 'tiles' }, tile(s.total, 'events on record'), tile(f.login_failure, 'failed sign-ins'), tile(f.lockout, 'account lockouts'),
          tile(f.forbidden, 'refused requests'), tile(f.token_rejected, 'rejected sessions')));
        summaryBox.appendChild(h('div', { class: 'card' },
          CG.barChart({ title: 'Audit events per hour, last 24 hours', height: 130, labels: s.series.map(function (p) { return new Date(p.t * 1000).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' }); }), series: [
            { name: 'normal events', cls: 's0', values: s.series.map(function (p) { return p.events - p.failures; }) },
            { name: 'failures and refusals', cls: 's2', values: s.series.map(function (p) { return p.failures; }) }] })));
        var types = Object.keys(s.by_type).slice(0, 10).map(function (k) { return { label: k, value: s.by_type[k], cls: /login_failure|lockout|forbidden|token_rejected/.test(k) ? 's2' : 's0' }; });
        summaryBox.appendChild(h('div', { class: 'two' },
          h('div', { class: 'card' }, h('h3', { text: 'What is logged most' }), CG.hbars(types, s.total)),
          h('div', { class: 'card' }, h('h3', { text: 'Most active people (last 24 hours)' }),
            s.actors.length ? CG.hbars(s.actors.map(function (a) { return { label: a.badge, value: a.events, cls: 's1' }; })) : h('p', { class: 'muted', text: 'Nobody yet.' }))));
      }, CG.fail);
    }
    function loadRows() {
      CG.api('GET', '/audit/events' + query()).then(function (d) {
        last = { rows: d.events, matched: d.matched };
        CG.clear(table); CG.clear(pager);
        if (!d.events.length) { table.appendChild(h('p', { class: 'muted', text: 'No events match.' })); }
        else table.appendChild(CG.table([
          { label: '#', num: true, get: function (r) { return r.sequence; } },
          { label: 'When', get: function (r) { return CG.when(r.recorded_at); } },
          { label: 'Event', get: function (r) { return CG.chip(r.event.event_type, /login_failure|lockout|forbidden|token_rejected/.test(r.event.event_type) ? 'high' : 'neutral'); } },
          { label: 'What happened', get: function (r) { return describe(r.event); } },
          { label: 'Claim', get: function (r) { return r.event.claim_id ? h('a', { href: '#/trace/' + encodeURIComponent(r.event.claim_id), class: 'mono', text: r.event.claim_id }) : null; } }
        ], d.events));
        pager.appendChild(h('button', { class: 'btn small', type: 'button', text: 'Newer', disabled: offset === 0, onclick: function () { offset = Math.max(0, offset - limit); loadRows(); } }));
        pager.appendChild(h('button', { class: 'btn small', type: 'button', text: 'Older', disabled: offset + d.events.length >= d.matched, onclick: function () { offset += limit; loadRows(); } }));
        pager.appendChild(h('span', { class: 'muted small', text: 'Showing ' + (offset + 1) + ' to ' + (offset + d.events.length) + ' of ' + d.matched }));
      }, CG.fail);
    }
    function exportCsv() {
      CG.api('GET', '/audit/events' + query({ limit: 1000, offset: 0 })).then(function (d) {
        var lines = ['sequence,recorded_at,event_type,actor,claim_id,description'];
        d.events.forEach(function (r) { lines.push([r.sequence, r.recorded_at, r.event.event_type, r.event.actor || r.event.badge_id, r.event.claim_id, describe(r.event)].map(csvCell).join(',')); });
        var a = h('a', { href: URL.createObjectURL(new Blob([lines.join('\n') + '\n'], { type: 'text/csv' })), download: 'claimguard-audit.csv' });
        document.body.appendChild(a); a.click(); a.remove();
        CG.toast('Exported ' + d.events.length + ' events (at most 1000 per file).', 'good');
      }, CG.fail);
    }
    var check = h('button', { class: 'btn', type: 'button', text: 'Verify the logs', onclick: function () {
      CG.api('GET', '/audit/verify').then(function (r) {
        CG.clear(verdict);
        var ok = r.security.ok && r.review.ok;
        verdict.appendChild(h('div', { class: 'notice ' + (ok ? 'good' : 'bad') },
          h('b', { text: ok ? 'Both logs are intact. ' : 'A log failed its check. ' }),
          'Security log: ' + (r.security.ok ? r.security.events + ' events, chain valid' : (r.security.error || 'failed')) + '. Review log: ' + (r.review.ok ? r.review.events + ' events, chain valid' : (r.review.error || 'failed')) + '.'));
      }, CG.fail);
    } });
    if (!CG.has('audit.verify')) check.disabled = true;
    var traceInput = h('input', { type: 'text', placeholder: 'Claim id, e.g. CG-1234ABCD', maxlength: '80', 'aria-label': 'Claim id to trace' });
    function go() { if (traceInput.value.trim()) location.hash = '#/trace/' + encodeURIComponent(traceInput.value.trim()); }
    traceInput.addEventListener('keydown', function (e) { if (e.key === 'Enter') go(); });
    [type, period].forEach(function (el) { el.addEventListener('change', function () { offset = 0; loadRows(); }); });
    [badge, claim].forEach(function (el) { el.addEventListener('keydown', function (e) { if (e.key === 'Enter') { offset = 0; loadRows(); } }); });

    root.appendChild(h('div', { class: 'row between' }, h('h1', { text: 'Audit' }), check));
    root.appendChild(verdict);
    root.appendChild(summaryBox);
    root.appendChild(h('div', { class: 'card' }, h('h3', { text: 'Follow one claim from intake to decision' }),
      h('div', { class: 'row' }, h('div', { class: 'grow' }, traceInput), h('button', { class: 'btn primary', type: 'button', text: 'Show its trace', onclick: go }))));
    root.appendChild(h('h2', { class: 'sect', text: 'Event log' }));
    root.appendChild(h('div', { class: 'row' }, h('div', { class: 'grow' }, type), h('div', null, period), h('div', null, badge), h('div', null, claim),
      h('button', { class: 'btn', type: 'button', text: 'Apply', onclick: function () { offset = 0; loadRows(); } }), h('button', { class: 'btn', type: 'button', text: 'Download CSV', onclick: exportCsv })));
    root.appendChild(h('div', { class: 'card' }, table, pager));
    loadSummary();
    loadRows();
  };

  // ---- trace ------------------------------------------------------------------------------------------------------------
  function shortHash(x) { return x ? h('span', { class: 'mono', title: x, text: String(x).slice(0, 12) }) : null; }

  CG.views.trace = function (root, claimId) {
    root.appendChild(h('div', { class: 'row between' }, h('div', null, h('a', { href: '#/audit', text: 'Back to the audit' }), h('h1', { text: 'Trace of ' + claimId, class: 'mono' }))));
    var body = h('p', { class: 'muted', text: 'Loading...' });
    root.appendChild(body);
    CG.api('GET', '/ops/trace/' + encodeURIComponent(claimId)).then(function (t) {
      CG.clear(body);
      var out = h('div');
      out.appendChild(h('div', { class: 'notice info', text: 'This is who did what and when, plus the hashes that tie each step to the exact claim, rule pack and settings used. It contains no claim values.' }));
      out.appendChild(h('div', { class: 'row' }, h('button', { class: 'btn', type: 'button', text: 'Download as JSON', onclick: function () {
        var a = h('a', { href: URL.createObjectURL(new Blob([JSON.stringify(t, null, 1)], { type: 'application/json' })), download: 'trace-' + claimId + '.json' });
        document.body.appendChild(a); a.click(); a.remove();
      } })));
      t.versions.forEach(function (v) {
        var r = v.receipt || {}, card = h('div', { class: 'card' });
        card.appendChild(h('div', { class: 'row between' }, h('h2', { text: 'Version ' + v.version }), h('div', { class: 'row' }, CG.chip(r.lane || '-', r.lane === 'A' || r.lane === 'B' || r.lane === 'green' ? r.lane : 'neutral'), CG.chip(v.state, 'neutral'))));
        card.appendChild(CG.kv([['Triaged', CG.fmtTime(r.created_at)], ['Score', r.score], ['Eligibility', r.eligibility && r.eligibility.replace(/_/g, ' ')], ['Settings version', r.config_version],
          ['Rule pack', shortHash(r.rule_pack_hash)], ['Engine', r.engine_version && h('span', { class: 'mono', text: String(r.engine_version).slice(0, 24) })], ['Input', shortHash(v.input_hash)], ['Results', shortHash(r.facts_hash)],
          ['Decided by', v.decided_by], ['Escalated', v.escalated ? 'yes' : null], ['Flagged', v.findings.length ? v.findings.map(function (f) { return f.rule_id + ' ' + f.status.replace(/_/g, ' ').toLowerCase(); }).join(', ') : 'nothing flagged']]));
        card.appendChild(h('h3', { text: 'State history' }));
        card.appendChild(CG.table([
          { label: 'When', get: function (e) { return CG.fmtTime(e.at); } },
          { label: 'Change', get: function (e) { return e.from + ' -> ' + e.to; } },
          { label: 'By', get: function (e) { return e.actor; } },
          { label: 'Detail', get: function (e) { return Object.keys(e.detail || {}).length ? h('span', { class: 'mono small', text: JSON.stringify(e.detail) }) : null; } }], v.events));
        if (v.decisions.length) {
          card.appendChild(h('h3', { text: 'Decisions' }));
          card.appendChild(CG.table([
            { label: 'When', get: function (d) { return d.at ? CG.fmtTime(d.at) : '-'; } }, { label: 'Who', get: function (d) { return d.actor; } },
            { label: 'Rule', get: function (d) { return d.rule_id; } }, { label: 'Action', get: function (d) { return String(d.action || '').replace(/_/g, ' '); } },
            { label: 'Reason', get: function (d) { return d.reason; } }], v.decisions));
        }
        out.appendChild(card);
      });
      out.appendChild(h('div', { class: 'card' }, h('h3', { text: 'Security log entries naming this claim' }), t.security_log.length ? CG.table([
        { label: '#', num: true, get: function (r) { return r.sequence; } }, { label: 'When', get: function (r) { return CG.when(r.recorded_at); } },
        { label: 'What happened', get: function (r) { return describe(r.event); } }], t.security_log) : h('p', { class: 'muted', text: 'None.' })));
      body.appendChild(out);
    }, function (e) { CG.clear(body); body.appendChild(h('div', { class: 'notice bad', text: e.status === 404 ? 'No claim with that id is on record.' : e.message })); });
  };
})();
