/* ClaimGuard console: the learning panel (administrators). Shows what reviewers' decisions have taught the system about each rule, the proposal
 * waiting for approval with its replay, and lets an administrator re-derive every number. Same rule as every file here: text only, never markup. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  CG.views = CG.views || {};

  var STATUS_TEXT = { unmeasured: 'Not enough decisions yet', measured: 'Reliable', doubtful: 'Doubtful (usually overridden)' };
  var STATUS_KIND = { unmeasured: 'info', measured: 'green', doubtful: 'medium' };

  function pct(v) { return (100 * v).toFixed(0) + '%'; }

  // an interval drawn as a bar: the line is the 95% interval, the dot the observed rate
  function bar(r) {
    var ns = 'http://www.w3.org/2000/svg', svg = document.createElementNS(ns, 'svg');
    svg.setAttribute('viewBox', '0 0 100 10'); svg.setAttribute('width', '160'); svg.setAttribute('height', '16'); svg.setAttribute('role', 'img');
    svg.setAttribute('aria-label', 'upheld rate ' + pct(r.rate) + ', interval ' + pct(r.lower) + ' to ' + pct(r.upper));
    function el(tag, a) { var e = document.createElementNS(ns, tag); Object.keys(a).forEach(function (k) { e.setAttribute(k, a[k]); }); svg.appendChild(e); }
    el('line', { x1: 0, y1: 5, x2: 100, y2: 5, stroke: 'currentColor', 'stroke-opacity': '0.15', 'stroke-width': 2 });
    el('line', { x1: 100 * r.lower, y1: 5, x2: 100 * r.upper, y2: 5, stroke: 'currentColor', 'stroke-width': 3 });
    el('circle', { cx: 100 * r.rate, cy: 5, r: 3, fill: 'currentColor' });
    return svg;
  }

  function rulesTable(rules) {
    var ids = Object.keys(rules || {}).sort();
    if (!ids.length) return h('p', { class: 'muted', text: 'No rule has been decided yet.' });
    return CG.table([
      { label: 'Rule', get: function (id) { return id; } },
      { label: 'Decisions', num: true, get: function (id) { return rules[id].n + ' by ' + rules[id].reviewers; } },
      { label: 'Upheld', num: true, get: function (id) { return pct(rules[id].rate); } },
      { label: '95% interval', get: function (id) { return h('span', null, bar(rules[id]), ' ' + pct(rules[id].lower) + ' to ' + pct(rules[id].upper)); } },
      { label: 'Status', get: function (id) { return h('span', { class: 'chip ' + STATUS_KIND[rules[id].status], text: STATUS_TEXT[rules[id].status] }); } }
    ], ids);
  }

  function replayCard(rep) {
    if (!rep) return null;
    var rows = [['Recent claims replayed', rep.claims], ['Need a senior now', rep.senior_before + ' (' + pct(rep.senior_share_before) + ')'],
      ['Would need a senior', rep.senior_after + ' (' + pct(rep.senior_share_after) + ')'], ['Verdicts changed', rep.verdicts_unchanged ? 'none' : 'YES - do not approve']];
    return h('div', { class: 'card' }, h('h3', { text: 'Replay: what this would change' }),
      h('dl', { class: 'kv' }, rows.map(function (r) { return [h('dt', { text: r[0] }), h('dd', { text: String(r[1]) })]; }).reduce(function (a, b) { return a.concat(b); }, [])),
      rep.note ? h('div', { class: 'notice warn', text: rep.note }) : null);
  }

  CG.views.learning = function (root) {
    var body = h('div'), msg = h('div');
    function load() {
      CG.api('GET', '/queue/reliability').then(function (r) {
        CG.clear(body);
        var s = r.settings;
        body.appendChild(h('div', { class: 'notice info' }, 'Learning is ' + (s.learn_enabled ? 'on' : 'off') + ' and routing by reliability is ' + (s.route_by_reliability ? 'on' : 'off') +
          '. A learned snapshot changes nothing until a second administrator approves it.'));
        body.appendChild(h('h2', { class: 'sect', text: r.active ? 'In use: version ' + r.active.version + (r.active.verified ? '' : ' (FAILED verification, ignored)') : 'No learned snapshot is in use' }));
        if (r.active) body.appendChild(rulesTable(r.active.rules));
        var latest = r.latest;
        if (latest && latest.status === 'proposed') {
          body.appendChild(h('h2', { class: 'sect', text: 'Waiting for approval: version ' + latest.version }));
          body.appendChild(rulesTable(latest.rules));
          body.appendChild(replayCard(latest.replay));
          body.appendChild(h('div', { class: 'row' },
            h('button', { class: 'btn', text: 'Re-derive every number', onclick: function () { verify(latest.version); } }),
            h('button', { class: 'btn primary', text: 'Ask for approval to use it', onclick: function () { activate(latest.version); } })));
        }
        body.appendChild(h('div', { class: 'row' }, h('button', { class: 'btn', text: 'Learn from decisions now', onclick: learn })));
      }).catch(function (e) { CG.clear(body); body.appendChild(h('div', { class: 'notice bad', text: (e && e.message ? e.message : 'Something went wrong.') })); });
    }
    function say(text, kind) { CG.clear(msg); msg.appendChild(h('div', { class: 'notice ' + kind, text: text })); }
    function learn() { CG.api('POST', '/queue/reliability/learn').then(function (r) { say(r.proposed ? 'Proposal ' + r.proposed + ' created.' : 'Nothing new to learn.', 'info'); load(); }).catch(function (e) { say((e && e.message ? e.message : 'Something went wrong.'), 'bad'); }); }
    function verify(v) {
      CG.api('GET', '/queue/reliability/' + v + '/verify').then(function (r) {
        var rows = Object.keys(r.rules).sort().map(function (id) { return { id: id, s: r.rules[id].stored, c: r.rules[id].recomputed }; });
        CG.clear(msg);
        msg.appendChild(h('div', { class: 'notice ' + (r.ok ? 'good' : 'bad'), text: r.ok ? 'Every figure was recomputed from the counts and matches.' : 'Mismatch: ' + r.problem }));
        msg.appendChild(CG.table([
          { label: 'Rule', get: function (x) { return x.id; } },
          { label: 'Stored interval', get: function (x) { return pct(x.s.lower) + ' to ' + pct(x.s.upper); } },
          { label: 'Recomputed', get: function (x) { return x.c ? pct(x.c.lower) + ' to ' + pct(x.c.upper) : '-'; } }
        ], rows));
      }).catch(function (e) { say((e && e.message ? e.message : 'Something went wrong.'), 'bad'); });
    }
    function activate(v) {
      CG.api('POST', '/queue/reliability/' + v + '/activate').then(function (r) {
        say(r.status === 'applied' ? 'Version ' + v + ' is now in use.' : 'Waiting for a second administrator to approve it under Users, approvals.', r.status === 'applied' ? 'good' : 'info');
        load();
      }).catch(function (e) { say((e && e.message ? e.message : 'Something went wrong.'), 'bad'); });
    }
    root.appendChild(h('h1', { text: 'What the system has learned' }));
    root.appendChild(msg);
    root.appendChild(body);
    load();
  };
})();
