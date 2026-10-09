/* ClaimGuard console: the operator's side (queue overview, routing settings, claim intake, users, audit). Level 4 only, by permission. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  CG.views = CG.views || {};

  function tile(n, label, kind) { return h('div', { class: 'tile' }, h('div', { class: 'n', text: String(n) }), h('div', { class: 'l', text: label }), kind ? CG.chip(kind, kind) : null); }
  function pct(x) { return x === null || x === undefined ? '-' : (Math.round(x * 1000) / 10) + '%'; }

  // ---- queue overview + feedback report ---------------------------------------------------------------------------------
  CG.views.queue = function (root) {
    var out = h('div', { class: 'stack' });
    root.appendChild(h('div', { class: 'row between' }, h('h1', { text: 'Queue overview' }), h('button', { class: 'btn', type: 'button', text: 'Refresh', onclick: load })));
    root.appendChild(out);
    function load() {
      Promise.all([CG.api('GET', '/queue/dashboard'), CG.api('GET', '/queue/feedback')]).then(function (res) {
        var d = res[0], f = res[1];
        CG.clear(out);
        if (d.shortages && d.shortages.length) out.appendChild(h('div', { class: 'notice bad' }, h('b', { text: 'Needs attention: ' }), d.shortages.join('; ')));
        out.appendChild(h('div', { class: 'tiles' },
          tile(d.waiting.decide, 'waiting for a reviewer'), tile(d.waiting.decide_high, 'waiting for a senior'), tile(d.awaiting_countersign, 'awaiting countersign'),
          tile(CG.fmtDuration(d.oldest_waiting_seconds.decide), 'oldest standard wait'), tile(CG.fmtDuration(d.oldest_waiting_seconds.decide_high), 'oldest senior wait'),
          tile(d.on_shift, 'on shift'), tile(d.on_shift_senior, 'seniors on shift')));
        var LANES = { A: 'lane A', B: 'lane B', green: 'green' }, STAGES = { decide: 'standard review', decide_high: 'senior review', countersign: 'countersign' };
        function stateLabel(key) {
          var p = key.split('|');
          if (p.length !== 3) return key.replace(/_/g, ' ');
          return p[0].charAt(0).toUpperCase() + p[0].slice(1).replace(/_/g, ' ') + ' - ' + (LANES[p[1]] || p[1]) + ' - ' + (STAGES[p[2]] || p[2].replace(/_/g, ' '));
        }
        var states = Object.keys(d.counts || {}).sort();
        out.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Claims by state' }),
          states.length ? CG.table([{ label: 'State', get: function (s) { return stateLabel(s); } }, { label: 'Claims', num: true, get: function (s) { return d.counts[s]; } }], states) : h('p', { class: 'muted', text: 'No claims yet. Use "Submit claims" to add some.' })));
        var badges = Object.keys(d.inbox_sizes || {}).sort();
        out.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Personal inboxes (target ' + d.slice_size + ' each)' }),
          badges.length ? CG.table([{ label: 'Reviewer', get: function (b) { return h('span', { class: 'mono', text: b }); } }, { label: 'In inbox', num: true, get: function (b) { return d.inbox_sizes[b]; } },
            { label: 'Fill', get: function (b) { var bar = h('div', { class: 'bar' }, h('i')); bar.firstChild.style.width = Math.min(100, 100 * d.inbox_sizes[b] / Math.max(1, d.slice_size)) + '%'; return bar; } }], badges)
            : h('p', { class: 'muted', text: 'Nobody is on shift. Add badges in Routing settings.' })));
        // HITL feedback (counts only)
        var rules = Object.keys(f.rules || {});
        out.appendChild(h('div', { class: 'card' }, h('h2', { text: 'What reviewers decided' }),
          h('p', { class: 'muted', text: f.note }),
          h('div', { class: 'tiles' }, tile(f.claims_decided, 'claims decided'), tile(f.verified_clear, 'verified clear'), tile(f.escalated, 'escalated'), tile(pct(f.signoff.disagreement_rate), 'senior disagreement')),
          rules.length ? CG.table([
            { label: 'Rule', get: function (r) { return h('b', { class: 'mono', text: r }); } },
            { label: 'Flagged', num: true, get: function (r) { return f.rules[r].flagged; } },
            { label: 'Confirmed', num: true, get: function (r) { return f.rules[r].confirmed; } },
            { label: 'Dismissed', num: true, get: function (r) { return f.rules[r].dismissed; } },
            { label: 'Dismissal rate', num: true, get: function (r) { return pct(f.rules[r].dismissal_rate); } },
            { label: '95% interval', num: true, get: function (r) { var s = f.rules[r]; return s.dismissal_rate_lower === null ? '-' : pct(s.dismissal_rate_lower) + ' to ' + pct(s.dismissal_rate_upper); } },
            { label: 'Look at rule?', get: function (r) { return f.rules[r].review_candidate ? CG.chip('review candidate', 'B') : null; } }], rules) : h('p', { class: 'muted', text: 'No decisions yet.' })));
      }, CG.fail);
    }
    load();
    CG.every(15000, load);
  };

  // ---- routing settings ---------------------------------------------------------------------------------------------------
  var NUMBERS = [
    ['slice_size', 'Inbox size per reviewer', 'How many claims each reviewer on shift holds at once.'],
    ['low_water', 'Top-up level', 'When an inbox drops to this many, the dispatcher deals more.'],
    ['lease_seconds', 'Lease length (seconds)', 'How long a reviewer may hold a claim without activity.'],
    ['aging_per_hour', 'Aging per hour', 'Extra priority a waiting claim gains each hour.'],
    ['lane_b_flagged', 'Lane B: flagged findings', 'This many flagged findings puts a claim in lane B.'],
    ['lane_b_score', 'Lane B: score', 'This score or more puts a claim in lane B.'],
    ['ai_per_minute', 'AI calls per minute', 'Rate cap on explanation drafts.'],
    ['ai_daily_budget', 'AI calls per day', 'Daily budget for explanation drafts.']
  ];
  var POINTS = [['FAIL', 'high'], ['FAIL', 'medium'], ['UNABLE_TO_ASSESS', 'high'], ['UNABLE_TO_ASSESS', 'medium']];

  CG.views.config = function (root) {
    root.appendChild(h('h1', { text: 'Routing settings' }));
    var body = h('div'); root.appendChild(body);
    CG.api('GET', '/queue/config').then(function (d) {
      var cfg = d.config, inputs = {}, pointInputs = {};
      var numberFields = NUMBERS.map(function (n) {
        inputs[n[0]] = h('input', { type: 'number', step: 'any', value: cfg[n[0]] });
        return h('div', { class: 'field' }, h('label', { text: n[1] }), inputs[n[0]], h('div', { class: 'muted small', text: n[2] }));
      });
      var pointFields = POINTS.map(function (p) {
        var key = p[0] + '/' + p[1];
        pointInputs[key] = h('input', { type: 'number', min: '0', step: '1', value: (cfg.points[p[0]] || {})[p[1]] });
        return h('div', { class: 'field' }, h('label', { text: p[0].replace(/_/g, ' ').toLowerCase() + ', ' + p[1] }), pointInputs[key]);
      });
      var shift = h('textarea', { placeholder: 'One badge per line, e.g. CG-2002' }); shift.value = (cfg.on_shift || []).join('\n');
      var excl = h('textarea', { placeholder: 'One pair per line: reviewer badge, patient id (that reviewer never gets that patient)' }); excl.value = (cfg.exclusions || []).map(function (p) { return p.join(', '); }).join('\n');
      var save = h('button', { class: 'btn primary', type: 'button', text: 'Save settings', onclick: function () {
        var changes = {}, k;
        NUMBERS.forEach(function (n) {
          var v = Number(inputs[n[0]].value);
          if (inputs[n[0]].value === '' || isNaN(v)) return;
          if (v !== cfg[n[0]]) changes[n[0]] = v;
        });
        var pts = JSON.parse(JSON.stringify(cfg.points)), pchanged = false;
        POINTS.forEach(function (p) { var v = Number(pointInputs[p[0] + '/' + p[1]].value); if (v !== pts[p[0]][p[1]]) { pts[p[0]][p[1]] = v; pchanged = true; } });
        if (pchanged) changes.points = pts;
        var badges = shift.value.split('\n').map(function (s) { return s.trim(); }).filter(Boolean);
        if (JSON.stringify(badges) !== JSON.stringify(cfg.on_shift || [])) changes.on_shift = badges;
        var pairs = excl.value.split('\n').map(function (s) { return s.split(',').map(function (x) { return x.trim(); }); }).filter(function (p) { return p.length === 2 && p[0] && p[1]; });
        if (JSON.stringify(pairs) !== JSON.stringify(cfg.exclusions || [])) changes.exclusions = pairs;
        for (k in changes) if (Object.prototype.hasOwnProperty.call(changes, k)) break;
        if (!k) { CG.toast('Nothing changed.', 'info'); return; }
        changes.expected_version = d.stored_version;
        CG.api('PATCH', '/queue/config', changes).then(function (r) { CG.toast('Saved as version ' + r.version + '.', 'good'); CG.clear(root); CG.views.config(root); }, CG.fail);
      } });
      body.appendChild(h('p', { class: 'muted', text: 'Version ' + d.stored_version + '. Every change is a new version in the audit trail. You set capacity and the formula here; you never assign a claim to a person.' }));
      body.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Who is on shift' }), h('div', { class: 'field' }, h('label', { text: 'Badges on shift' }), shift), h('div', { class: 'field' }, h('label', { text: 'Conflict-of-interest exclusions' }), excl)));
      body.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Capacity and timing' }), h('div', { class: 'row' }, numberFields.map(function (f) { return h('div', { style: null, class: 'grow' }, f); }))));
      body.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Points per flagged finding' }), h('div', { class: 'row' }, pointFields.map(function (f) { return h('div', { class: 'grow' }, f); }))));
      body.appendChild(h('div', { class: 'row', style: null }, save));
    }, function (e) { body.appendChild(h('div', { class: 'notice bad', text: e.message })); });
  };

  // ---- submit claims --------------------------------------------------------------------------------------------------------
  function parseClaims(text) {
    text = text.trim();
    if (!text) return [];
    if (text[0] === '[') { var arr = JSON.parse(text); if (!Array.isArray(arr)) throw new Error('Expected a list of claims.'); return arr; }
    return text.split('\n').filter(function (l) { return l.trim(); }).map(function (l, i) {
      try { return JSON.parse(l); } catch (e) { throw new Error('Line ' + (i + 1) + ' is not valid JSON.'); }
    });
  }

  CG.views.submit = function (root) {
    var area = h('textarea', { placeholder: 'Paste claims here: one JSON object per line (JSONL), or a JSON list.', rows: '8' });
    var file = h('input', { type: 'file', accept: '.jsonl,.json,.txt,application/json', 'aria-label': 'Claims file' });
    var out = h('div'), go = h('button', { class: 'btn primary', type: 'button', text: 'Submit claims' });
    file.addEventListener('change', function () {
      var f = file.files && file.files[0]; if (!f) return;
      var r = new FileReader(); r.onload = function () { area.value = String(r.result); }; r.readAsText(f);
    });
    go.addEventListener('click', function () {
      var claims;
      try { claims = parseClaims(area.value); } catch (e) { CG.toast(e.message, 'bad'); return; }
      if (!claims.length) { CG.toast('There are no claims to submit.', 'warn'); return; }
      go.disabled = true; CG.clear(out);
      var rows = [], i = 0, progress = h('p', { class: 'muted' }); out.appendChild(progress);
      (function next() {
        if (i >= claims.length) {
          go.disabled = false;
          var ok = rows.filter(function (r) { return r.accepted; }).length;
          CG.clear(out);
          out.appendChild(h('div', { class: 'notice ' + (ok === rows.length ? 'good' : 'warn'), text: ok + ' of ' + rows.length + ' claims accepted. Workers pick them up within seconds; reviewers see them once the dispatcher deals them.' }));
          var bad = rows.filter(function (r) { return !r.accepted; });
          if (bad.length) out.appendChild(CG.table([{ label: 'Claim', get: function (r) { return r.claim_id; } }, { label: 'Why it was refused', get: function (r) { return r.reason; } }], bad));
          var lanes = {}; rows.forEach(function (r) { if (r.accepted) lanes[r.lane] = (lanes[r.lane] || 0) + 1; });
          out.appendChild(h('p', { class: 'muted', text: 'Triage lanes: ' + Object.keys(lanes).sort().map(function (l) { return l + ' ' + lanes[l]; }).join(', ') }));
          return;
        }
        var batch = claims.slice(i, i + 20); i += 20;
        progress.textContent = 'Sending ' + Math.min(i, claims.length) + ' of ' + claims.length + '...';
        CG.api('POST', '/queue/submit', { claims: batch }).then(function (r) { rows = rows.concat(r.results); next(); }, function (e) { go.disabled = false; CG.fail(e); });
      })();
    });
    root.appendChild(h('h1', { text: 'Submit claims' }));
    root.appendChild(h('div', { class: 'card stack' }, h('p', { class: 'muted', text: 'Each claim goes through the same intake as the command-line tool: it is checked, run through the 15 rules and given a triage receipt. Sending the same claim twice stores it once.' }),
      h('div', { class: 'field' }, h('label', { text: 'File (optional)' }), file), h('div', { class: 'field' }, h('label', { text: 'Claims' }), area), go));
    root.appendChild(out);
  };

  // ---- users ------------------------------------------------------------------------------------------------------------------
  function provisioning(uri, badge) {
    var qr = CG.qr(uri);
    return h('div', { class: 'card stack' }, h('h3', { text: 'Authenticator setup for ' + badge }),
      h('div', { class: 'notice warn', text: 'Shown once. Scan it now with an authenticator app; it will not be shown again.' }),
      qr, h('div', { class: 'mono small', style: null, text: uri }));
  }

  CG.views.users = function (root) {
    var list = h('div'), extra = h('div');
    var LEVELS = { 1: '1 Viewer', 2: '2 Reviewer', 3: '3 Senior reviewer', 4: '4 Administrator' };
    function load() {
      CG.api('GET', '/users').then(function (d) {
        CG.clear(list);
        list.appendChild(CG.table([
          { label: 'Badge', get: function (u) { return h('b', { class: 'mono', text: u.badge_id }); } }, { label: 'Name', get: function (u) { return u.name; } },
          { label: 'Level', get: function (u) { return LEVELS[u.level]; } },
          { label: 'State', get: function (u) { return [u.active ? CG.chip('active', 'green') : CG.chip('inactive', 'neutral'), u.locked ? CG.chip('locked', 'high') : null, u.must_change_password ? CG.chip('new password due', 'B') : null]; } },
          { label: 'Last sign-in', get: function (u) { return u.last_login ? CG.fmtTime(u.last_login) : 'never'; } },
          { label: '', get: function (u) {
            var me = u.badge_id === CG.state.me.badge_id;
            return h('div', { class: 'row' },
              u.locked ? h('button', { class: 'btn small', type: 'button', text: 'Unlock', onclick: function () { act(CG.api('POST', '/users/' + encodeURIComponent(u.badge_id) + '/unlock')); } }) : null,
              h('button', { class: 'btn small', type: 'button', text: 'Reset authenticator', onclick: function () {
                CG.api('POST', '/users/' + encodeURIComponent(u.badge_id) + '/reset-totp').then(function (r) { CG.clear(extra); extra.appendChild(provisioning(r.provisioning_uri, u.badge_id)); load(); }, CG.fail);
              } }),
              me ? null : h('button', { class: 'btn small ' + (u.active ? 'danger' : ''), type: 'button', text: u.active ? 'Deactivate' : 'Activate', onclick: function () { act(CG.api('PATCH', '/users/' + encodeURIComponent(u.badge_id), { active: !u.active })); } }));
          } }
        ], d.users));
      }, CG.fail);
    }
    function act(p) { p.then(function () { CG.toast('Done.', 'good'); load(); }, CG.fail); }

    var badge = h('input', { type: 'text', maxlength: '64', placeholder: 'CG-2010', autocomplete: 'off' });
    var name = h('input', { type: 'text', maxlength: '200', autocomplete: 'off' });
    var pw = h('input', { type: 'text', maxlength: '128', autocomplete: 'off', placeholder: 'A temporary password' });
    var level = h('select', null, [1, 2, 3, 4].map(function (l) { return h('option', { value: l, text: LEVELS[l] }); }));
    level.value = '2';
    var create = h('button', { class: 'btn primary', type: 'button', text: 'Create user', onclick: function () {
      CG.api('POST', '/users', { badge_id: badge.value.trim(), name: name.value.trim(), password: pw.value, level: Number(level.value) }).then(function (r) {
        CG.clear(extra); extra.appendChild(provisioning(r.provisioning_uri, r.badge_id));
        badge.value = name.value = pw.value = ''; load();
      }, CG.fail);
    } });
    root.appendChild(h('h1', { text: 'Users' }));
    root.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Add a person' }),
      h('div', { class: 'row' }, h('div', { class: 'grow' }, h('label', { text: 'Badge' }), badge), h('div', { class: 'grow' }, h('label', { text: 'Name' }), name), h('div', { class: 'grow' }, h('label', { text: 'Temporary password' }), pw), h('div', null, h('label', { text: 'Level' }), level)),
      h('p', { class: 'muted small', text: 'They must change the password at first sign-in. Level 4 cannot decide claims, so whoever runs the system never approves its findings.' }), create));
    root.appendChild(extra);
    root.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Everyone' }), list));
    load();
  };

  // ---- audit ------------------------------------------------------------------------------------------------------------------
})();
