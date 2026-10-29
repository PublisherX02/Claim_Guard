/* ClaimGuard console: the person's own pages. Home is a different dashboard per clearance level (viewer, reviewer, senior, supervisor),
 * built from widgets the person may reorder or hide, and only from routes that level may already read. Customize edits the personal
 * template (CG.prefs): looks and layout, never permissions. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  CG.views = CG.views || {};

  function tile(n, label, hl) { return h('div', { class: 'tile' + (hl ? ' hl-' + hl : '') }, h('div', { class: 'n', text: String(n) }), h('div', { class: 'l', text: label })); }
  function linkCard(href, icon, title, text) { return h('a', { class: 'linkcard', href: href }, h('span', { class: 'ic-wrap' }, CG.icon(icon)), h('div', null, h('b', { text: title }), h('span', { text: text }))); }
  function greeting() { var hr = new Date().getHours(); return hr < 12 ? 'Good morning' : hr < 18 ? 'Good afternoon' : 'Good evening'; }
  function firstName() { return String((CG.state.me && CG.state.me.name) || '').split(/\s+/)[0] || ''; }
  function isOpen(f) { return f.allowed_actions && f.allowed_actions.length; }
  function flagged(f) { return f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS'; }
  function hero(sub, actions) {
    var me = CG.state.me, role = CG.ROLES[me.level];
    return h('section', { class: 'hero' },
      h('div', null, CG.levelBadge(me.level, { large: true }),
        h('h1', { text: CG.prefs.get('greeting') ? greeting() + ', ' + firstName() : role.label + ' workspace' }), h('p', { text: sub })),
      actions ? h('div', { class: 'row' }, actions) : null);
  }
  function takeNext() {
    return h('button', { class: 'btn primary', type: 'button', onclick: function () {
      CG.api('POST', '/work/next').then(function (d) {
        if (!d.claim) { CG.toast('Nothing is waiting for you right now.', 'info'); return; }
        location.hash = '#/work/' + encodeURIComponent(d.claim.claim_id);
      }, CG.fail);
    } }, CG.icon('bolt'), 'Take next claim');
  }

  // ---- the clearance card: the four levels as a ladder with this person's rung marked, and what it does and does not allow
  CG.clearanceCard = function () {
    var me = CG.state.me, perms = me.permissions || [];
    var ladder = h('div', { class: 'ladder', role: 'list' }, [4, 3, 2, 1].map(function (l) {
      var r = CG.ROLES[l];
      return h('div', { class: 'rung' + (l === me.level ? ' me' : l < me.level && me.level !== 4 ? ' below' : ''), role: 'listitem', 'aria-current': l === me.level ? 'true' : null },
        CG.levelBadge(l), h('span', null, h('b', { text: r.label }), l === me.level ? h('span', { class: 'you', text: 'You' }) : null), h('span', { class: 'rs', text: r.scope }));
    }));
    var can = Object.keys(CG.PERMS).filter(function (p) { return perms.indexOf(p) >= 0; });
    var cannot = Object.keys(CG.PERMS).filter(function (p) { return perms.indexOf(p) < 0; });
    function list(items, cls, icon) { return h('ul', { class: cls }, items.map(function (p) { return h('li', null, CG.icon(icon), h('span', { text: CG.PERMS[p] })); })); }
    return h('div', { class: 'card' },
      h('div', { class: 'row between' }, h('h2', { text: 'Your clearance' }), h('span', { class: 'muted small', text: me.badge_id + ' · level ' + me.level + ' of 4' })),
      ladder,
      h('div', { class: 'can' },
        h('div', null, h('h3', { text: 'You can' }), list(can, 'yes', 'check')),
        h('div', null, h('h3', { text: 'Not in your level' }), cannot.length ? list(cannot, 'no', 'lock') : h('p', { class: 'muted small', text: 'Nothing.' }))),
      me.level === 4 ? h('p', { class: 'muted small', text: 'Separation of duties: whoever administers the system does not approve its findings.' }) : null,
      h('p', { class: 'muted small', text: 'Your clearance is set by an administrator and checked by the server on every request.' }));
  };

  // widgets are drawn in the person's order
  function drawWidgets(host, kind, make) {
    var order = CG.prefs.get('homeWidgets').filter(function (w) { return CG.WIDGETS[kind].some(function (x) { return x[0] === w; }); });
    CG.clear(host);
    order.forEach(function (w) { var el = make(w); if (el) host.appendChild(el); });
    if (!order.length) host.appendChild(CG.empty('palette', 'Your Home is empty', 'Every widget is hidden. Turn some back on in Customize, Home tab.', h('a', { class: 'btn', href: '#/customize', text: 'Customize' })));
  }

  // ---- reviewer and senior: the work waiting for them --------------------------------------------------------------------
  function workHome(root, senior) {
    var role = CG.ROLES[CG.state.me.level];
    root.appendChild(hero(role.tagline + '. The engine sets each status; you decide what happens next.',
      [h('a', { class: 'btn', href: '#/inbox' }, CG.icon('inbox'), 'Open my inbox'), takeNext()]));
    var body = h('div', { class: 'stack' }); body.appendChild(CG.loading('Reading your inbox...'));
    root.appendChild(body);
    function load() {
      CG.api('GET', '/work/inbox').then(function (d) {
        var claims = d.claims || [], open = 0, green = 0, stage = 0, high = 0, byRule = {};
        claims.forEach(function (v) {
          if (!v.findings.filter(flagged).length) green++;
          if (v.signoff_stage) stage++;
          v.findings.forEach(function (f) { if (!isOpen(f)) return; open++; if (f.severity === 'high') high++; byRule[f.rule_id] = (byRule[f.rule_id] || 0) + 1; });
        });
        var rules = Object.keys(byRule).sort(function (a, b) { return byRule[b] - byRule[a]; });
        drawWidgets(body, 'work', function (w) {
          if (w === 'clearance') return CG.clearanceCard();
          if (w === 'tiles') return h('div', null,
            senior && stage ? h('div', { class: 'notice warn' }, h('b', { text: stage + ' claim' + (stage > 1 ? 's' : '') + ' need a second senior. ' }), 'High-severity findings are settled only when two different seniors agree; a third settles a disagreement.') : null,
            h('div', { class: 'tiles' }, tile(claims.length, 'claims in your inbox'), tile(open, 'findings to decide', open ? 'high' : 'ok'),
              tile(high, 'high severity', high ? 'high' : null), tile(green, 'clean claims to verify', green ? 'ok' : null),
              senior ? tile(stage, 'countersign or tie-break', stage ? 'med' : null) : null));
          if (w === 'rules') return h('div', { class: 'card' }, h('h2', { text: 'Open findings by rule' }),
            rules.length ? CG.hbars(rules.map(function (r) { return { label: r + ' ' + CG.ruleName(r), value: byRule[r], cls: 's1' }; }), open)
              : h('p', { class: 'muted', text: claims.length ? 'Nothing to decide: only clean claims to verify.' : 'Your inbox is empty. The dispatcher tops it up while you are on shift.' }));
          if (w === 'keys') return CG.prefs.get('keyHints') ? h('div', { class: 'card' }, h('h2', { text: 'Keyboard' }),
            h('ul', { class: 'plain hint-keys' },
              h('li', null, h('kbd', { text: 'c' }), ' ', h('kbd', { text: 'c' }), ' confirm every finding on the open claim (twice within 5 seconds)'),
              h('li', null, h('kbd', { text: 'Tab' }), ' move between findings and buttons; ', h('kbd', { text: 'Enter' }), ' opens a row'))) : null;
          if (w === 'links') return h('div', { class: 'links' },
            linkCard('#/claims', 'list', 'All claims', 'Browse and filter every claim'), linkCard('#/ask', 'book', 'Ask the rulebook', 'Questions answered with citations'),
            linkCard('#/customize', 'palette', 'Customize', 'Theme, layout and what opens first'));
          return null;
        });
      }, function (e) { CG.clear(body); body.appendChild(h('div', { class: 'notice bad', text: e.message })); });
    }
    load(); CG.every(30000, load); CG.live('inbox', load);
  }

  // ---- viewer: read-only, browse first --------------------------------------------------------------------------------------
  function viewerHome(root) {
    root.appendChild(hero('You have read-only access. You can read every claim, finding and its evidence; reviewers make the decisions.',
      [h('a', { class: 'btn primary', href: '#/claims' }, CG.icon('list'), 'Browse claims')]));
    var body = h('div', { class: 'stack' });
    root.appendChild(body);
    function go(status) { return function (e) { e.preventDefault(); CG.state.claimsFilter = { status: status }; location.hash = '#/claims'; }; }
    var recent = h('div', { class: 'card' }, h('h2', { text: 'Recently flagged claims' }), CG.loading());
    CG.api('GET', '/claims?limit=8&offset=0&status=FAIL').then(function (d) {
      recent.removeChild(recent.lastChild);
      if (!d.claims.length) { recent.appendChild(h('p', { class: 'muted', text: 'No claim has a failed rule.' })); return; }
      recent.appendChild(CG.table([
        { label: 'Claim', get: function (r) { return h('span', { class: 'mono cid', text: r.claim_id }); } },
        { label: 'Highest severity', get: function (r) { return r.max_severity ? CG.chip(r.max_severity) : null; } },
        { label: 'Results', get: function (r) { return Object.keys(r.counts || {}).sort().map(function (k) { return r.counts[k] + ' ' + k.replace(/_/g, ' ').toLowerCase(); }).join(', '); } }
      ], d.claims, { onRow: function (r) { location.hash = '#/claim/' + encodeURIComponent(r.claim_id); } }));
    }, function (e) { recent.removeChild(recent.lastChild); recent.appendChild(h('div', { class: 'notice bad', text: e.message })); });
    drawWidgets(body, 'viewer', function (w) {
      if (w === 'clearance') return CG.clearanceCard();
      if (w === 'shortcuts') return h('div', { class: 'links' },
        h('a', { class: 'linkcard', href: '#/claims', onclick: go('FAIL') }, h('span', { class: 'ic-wrap' }, CG.icon('shield')), h('div', null, h('b', { text: 'Claims with a failed rule' }), h('span', { text: 'Findings a reviewer must decide' }))),
        h('a', { class: 'linkcard', href: '#/claims', onclick: go('UNABLE_TO_ASSESS') }, h('span', { class: 'ic-wrap' }, CG.icon('eye')), h('div', null, h('b', { text: 'Could not be assessed' }), h('span', { text: 'Missing or unknown evidence' }))),
        linkCard('#/ask', 'book', 'Ask the rulebook', 'What each rule checks and why'));
      if (w === 'recent') return recent;
      return null;
    });
  }

  // ---- supervisor (administrator): the state of the operation ----------------------------------------------------------------
  function supervisorHome(root) {
    root.appendChild(hero('Keep the queue moving, the rules current and the audit trail intact. Separation of duties: you cannot decide claims.',
      [h('a', { class: 'btn', href: '#/health' }, CG.icon('pulse'), 'System health'), h('a', { class: 'btn primary', href: '#/queue' }, CG.icon('grid'), 'Queue overview')]));
    var body = h('div', { class: 'stack' }); body.appendChild(CG.loading('Reading the queue...'));
    root.appendChild(body);
    var TOOL_TEXT = { '#/import': 'PDF, photo, FHIR, CSV or JSONL', '#/batches': 'Audit samples and held batches', '#/config': 'Shifts, lanes and the routing formula',
      '#/rules': 'Limits and lists, with two-person approval', '#/audit': 'Verify the logs, trace a claim', '#/users': 'Accounts, authenticators, clearance' };
    function load() {
      CG.api('GET', '/queue/dashboard').then(function (d) {
        var w0 = d.waiting || {}, old = d.oldest_waiting_seconds || {}, badges = Object.keys(d.inbox_sizes || {}).sort();
        drawWidgets(body, 'admin', function (w) {
          if (w === 'clearance') return CG.clearanceCard();
          if (w === 'alerts') return d.shortages && d.shortages.length ? h('div', { class: 'notice bad' }, h('b', { text: 'Needs attention: ' }), d.shortages.join('; ')) : h('div', { class: 'notice good', text: 'No staffing shortage reported.' });
          if (w === 'tiles') return h('div', { class: 'tiles' },
            tile(w0.decide || 0, 'waiting for a reviewer', w0.decide ? 'info' : null), tile(w0.decide_high || 0, 'waiting for a senior', w0.decide_high ? 'med' : null),
            tile(d.awaiting_countersign || 0, 'awaiting countersign'), tile(CG.fmtDuration(Math.max(old.decide || 0, old.decide_high || 0)), 'oldest wait'),
            tile((d.on_shift || 0) + ' / ' + (d.on_shift_senior || 0), 'on shift / seniors'));
          if (w === 'fill') return badges.length ? h('div', { class: 'card' }, h('h2', { text: 'Inbox fill (target ' + d.slice_size + ' each)' }),
            CG.hbars(badges.map(function (b) { return { label: b, value: d.inbox_sizes[b], cls: 's0' }; }))) : null;
          if (w === 'links') return h('div', { class: 'links' }, CG.navItems().filter(function (i) { return TOOL_TEXT[i[0]]; }).map(function (i) { return linkCard(i[0], i[3], i[1], TOOL_TEXT[i[0]]); }));
          return null;
        });
      }, function (e) { CG.clear(body); body.appendChild(h('div', { class: 'notice bad', text: e.message })); });
    }
    load(); CG.every(30000, load); CG.live('queue', load);
  }

  CG.views.home = function (root) {
    CG.setTitle('Home');
    var kind = CG.homeKind();
    if (kind === 'work') workHome(root, CG.state.me.level >= 3);
    else if (kind === 'admin') supervisorHome(root);
    else viewerHome(root);
  };

  // ---- Customize: the personal template, in tabs ----------------------------------------------------------------------------
  CG.views.customize = function (root) {
    var me = CG.state.me, role = CG.ROLES[me.level], tab = CG.state.customTab || 'look';
    var status = h('span', { class: 'muted small', role: 'status' });
    function paintStatus() { var n = Object.keys(CG.prefs.custom()).length; status.textContent = n ? n + ' setting' + (n > 1 ? 's' : '') + ' differ from the ' + role.label.toLowerCase() + ' template' : 'Using the ' + role.label.toLowerCase() + ' template'; }
    function save(k, v) { var c = {}; c[k] = v; CG.prefs.save(c); paintStatus(); }
    function redraw() { if (CG.rerender) CG.rerender(); else { CG.clear(root); CG.views.customize(root); } }
    function saveShell(k, v) { save(k, v); redraw(); }      // settings that change the shell (menu, rail, clearance bar) redraw it

    function pref(title, text, control) { return h('div', { class: 'pref' }, h('div', { class: 'pl' }, h('b', { text: title }), h('span', { text: text })), control); }
    function seg(key, options) {
      var cur = CG.prefs.get(key), box = h('div', { class: 'seg', role: 'group', 'aria-label': key });
      options.forEach(function (o) {
        var b = h('button', { class: 'btn small', type: 'button', 'aria-pressed': String(cur === o[0]), text: o[1] });
        b.addEventListener('click', function () { save(key, o[0]); Array.prototype.forEach.call(box.children, function (x) { x.setAttribute('aria-pressed', String(x === b)); }); });
        box.appendChild(b);
      });
      return box;
    }
    function toggle(key, label, shell) {
      var s = h('input', { type: 'checkbox', class: 'switch', role: 'switch', 'aria-label': label });
      s.checked = !!CG.prefs.get(key);
      s.addEventListener('change', function () { if (shell) saveShell(key, s.checked); else save(key, s.checked); });
      return s;
    }
    function accentPicker() {
      var cur = CG.prefs.get('accent'), box = h('div', { class: 'colorpick' });
      var sw = h('div', { class: 'swatches', role: 'group', 'aria-label': 'Accent colour' });
      ['brand', 'teal', 'blue', 'violet', 'amber', 'rose', 'slate'].forEach(function (c) {
        var b = h('button', { class: 'swatchbtn ' + c, type: 'button', 'aria-pressed': String(cur === c), 'aria-label': c, title: c });
        b.addEventListener('click', function () { save('accent', c); Array.prototype.forEach.call(sw.children, function (x) { x.setAttribute('aria-pressed', String(x === b)); }); });
        sw.appendChild(b);
      });
      var hex = CG.prefs.get('accentHex') || '#5546e0';
      var picker = h('input', { type: 'color', value: hex, 'aria-label': 'Your own accent colour', title: 'Your own colour' });
      var code = h('input', { type: 'text', value: hex, maxlength: '7', 'aria-label': 'Accent colour code', spellcheck: 'false' });
      function useHex(v) {
        if (!/^#[0-9a-fA-F]{6}$/.test(v)) { CG.toast('Write a colour as # and six hex digits, e.g. #1d5fd1.', 'warn'); return; }
        CG.prefs.save({ accent: 'custom', accentHex: v }); picker.value = v.toLowerCase(); code.value = v.toLowerCase(); paintStatus();
        Array.prototype.forEach.call(sw.children, function (x) { x.setAttribute('aria-pressed', 'false'); });
      }
      picker.addEventListener('input', function () { useHex(picker.value); });
      code.addEventListener('change', function () { useHex(code.value.trim()); });
      box.appendChild(sw); box.appendChild(picker); box.appendChild(code);
      return box;
    }

    function lookPane() {
      return h('div', { class: 'card' }, h('h2', { text: 'Look and feel' }),
        pref('Theme', 'Follow the system, or always light or dark.', seg('theme', [['system', 'System'], ['light', 'Light'], ['dark', 'Dark']])),
        pref('Accent colour', 'Buttons, links and highlights. Pick a preset or any colour; the text on it stays readable.', accentPicker()),
        pref('Menu style', 'Your level’s colours, dark, light, or tinted with your accent.', seg('sidebar', [['role', 'Level'], ['dark', 'Dark'], ['light', 'Light'], ['accent', 'Accent']])),
        pref('Corners', 'Square, rounded or round edges on cards and buttons.', seg('corners', [['sharp', 'Square'], ['rounded', 'Rounded'], ['round', 'Round']])),
        pref('Typeface', 'Fonts already on this computer; nothing is downloaded.', seg('font', [['system', 'System'], ['readable', 'Easy to read'], ['humanist', 'Humanist'], ['serif', 'Serif'], ['mono', 'Mono']])),
        pref('Text size', 'Larger text for long sessions.', seg('text', [['s', 'S'], ['m', 'M'], ['l', 'L'], ['xl', 'XL']])),
        pref('Density', 'Compact fits more rows on a screen.', seg('density', [['comfortable', 'Comfortable'], ['compact', 'Compact']])),
        pref('Item focus', 'How much claims, findings and rules stand out from the rest of the page. Spotlight also dims the other findings while you work on one.', seg('focus', [['normal', 'Normal'], ['strong', 'Strong'], ['spotlight', 'Spotlight']])),
        pref('Contrast', 'High contrast strengthens borders and text.', seg('contrast', [['normal', 'Normal'], ['high', 'High']])),
        pref('Animation', 'Follow the system, or turn every animation off.', seg('motion', [['system', 'System'], ['reduce', 'Off']])),
        h('p', { class: 'muted small', text: 'Your clearance colour (the band on top of each page, your level badge) never changes, so your level stays recognisable whatever you choose. Colour blindness, reading and focus aids are in the Accessibility tab.' }));
    }
    function navPane() {
      var items = CG.navItems();
      var homeSel = h('select', { 'aria-label': 'Home page' }, items.filter(function (i) { return i[0] !== '#/customize'; }).map(function (i) {
        var o = h('option', { value: i[0], text: i[1] }); if (CG.prefs.get('home') === i[0]) o.selected = true; return o;
      }));
      homeSel.addEventListener('change', function () { save('home', homeSel.value); });
      var hidden = CG.prefs.get('hiddenNav') || [];
      var pick = h('div', { class: 'navpick' }, items.filter(function (i) { return i[0] !== '#/customize' && i[0] !== '#/home'; }).map(function (i) {
        var c = h('input', { type: 'checkbox' }); c.checked = hidden.indexOf(i[0]) < 0;
        c.addEventListener('change', function () {
          var now = (CG.prefs.get('hiddenNav') || []).filter(function (x) { return x !== i[0]; });
          if (!c.checked) now.push(i[0]);
          saveShell('hiddenNav', now);
        });
        return h('label', null, c, CG.icon(i[3]), h('span', { text: i[1] }));
      }));
      return h('div', { class: 'card' }, h('h2', { text: 'Layout and navigation' }),
        pref('Home page', 'The page that opens after you sign in.', homeSel),
        pref('Narrow menu', 'Show the menu as icons only; names appear on hover.', toggle('rail', 'Narrow menu', true)),
        pref('Clearance bar', 'The strip on top of every page with your level and what it allows.', toggle('idbar', 'Clearance bar', true)),
        pref('Greeting', 'Greet you by name on Home, or show the workspace name.', toggle('greeting', 'Greeting')),
        h('div', { class: 'pref' }, h('div', { class: 'pl' }, h('b', { text: 'Menu items' }), h('span', { text: 'Untick what you never use. Hiding is cosmetic: your access is unchanged and the page still opens from a link.' }), pick)));
    }
    function claimPane() {
      var c = h('div', { class: 'card' }, h('h2', { text: 'Claim page' }),
        pref('Layout', 'Claim facts beside the findings, or on top of them.', seg('layout', [['side', 'Side by side'], ['stacked', 'Stacked']])),
        pref('Open the evidence', 'Show each finding’s evidence table without a click (up to 6 rows).', toggle('evidenceOpen', 'Open the evidence')),
        pref('Show AI drafts', 'Plain-language drafts beside the engine’s explanation. They never change a status.', toggle('showAi', 'Show AI drafts')),
        pref('Show the corrective action', 'The rulebook’s “what to do” box on each finding.', toggle('showTodo', 'Show the corrective action')),
        pref('List passed rules', 'Show the rules that passed or did not apply, open.', toggle('showPassed', 'List passed rules')));
      if (CG.has('claims.decide')) c.appendChild(pref('Keyboard hints', 'Show shortcut reminders on claims and on Home.', toggle('keyHints', 'Keyboard hints')));
      return c;
    }
    function listsPane() {
      var c = h('div', { class: 'card' }, h('h2', { text: 'Lists and tables' }),
        pref('Striped rows', 'Alternate row shading for long tables.', toggle('zebra', 'Striped rows')),
        pref('Sticky headers', 'Keep column names visible while scrolling.', toggle('stickyHead', 'Sticky headers')),
        pref('Underline links', 'Underline every link, not only on hover.', toggle('underline', 'Underline links')));
      if (CG.has('claims.decide')) {
        c.appendChild(pref('Inbox view', 'Group your inbox claim by claim, or rule by rule.', seg('inboxMode', [['claim', 'By claim'], ['rule', 'By rule']])));
        var hiddenCols = CG.prefs.get('hiddenCols') || [];
        c.appendChild(h('div', { class: 'pref' }, h('div', { class: 'pl' }, h('b', { text: 'Inbox columns' }), h('span', { text: 'The claim number is always shown.' }),
          h('div', { class: 'navpick' }, CG.INBOX_COLS.map(function (col) {
            var cb = h('input', { type: 'checkbox' }); cb.checked = hiddenCols.indexOf(col[0]) < 0;
            cb.addEventListener('change', function () {
              var now = (CG.prefs.get('hiddenCols') || []).filter(function (x) { return x !== col[0]; });
              if (!cb.checked) now.push(col[0]);
              save('hiddenCols', now);
            });
            return h('label', null, cb, h('span', { text: col[1] }));
          })))));
      }
      return c;
    }
    function homePane() {
      var kind = CG.homeKind(), all = CG.WIDGETS[kind], list = h('ol', { class: 'order' });
      function draw() {
        var order = CG.prefs.get('homeWidgets').filter(function (w) { return all.some(function (x) { return x[0] === w; }); });
        var rows = order.concat(all.map(function (x) { return x[0]; }).filter(function (w) { return order.indexOf(w) < 0; }));
        CG.clear(list);
        rows.forEach(function (w) {
          var on = order.indexOf(w) >= 0, label = all.filter(function (x) { return x[0] === w; })[0][1], i = order.indexOf(w);
          var sw = h('input', { type: 'checkbox', class: 'switch', role: 'switch', 'aria-label': 'Show ' + label }); sw.checked = on;
          sw.addEventListener('change', function () { save('homeWidgets', sw.checked ? order.concat([w]) : order.filter(function (x) { return x !== w; })); draw(); });
          function move(d) { var o = order.slice(), j = i + d; if (j < 0 || j >= o.length) return; o[i] = o[j]; o[j] = w; save('homeWidgets', o); draw(); }
          list.appendChild(h('li', { class: on ? '' : 'off' }, sw, h('span', { class: 'grow', text: label }),
            h('button', { class: 'btn small', type: 'button', 'aria-label': 'Move ' + label + ' up', disabled: !on || i === 0, onclick: function () { move(-1); }, text: '↑' }),
            h('button', { class: 'btn small', type: 'button', 'aria-label': 'Move ' + label + ' down', disabled: !on || i === order.length - 1, onclick: function () { move(1); }, text: '↓' })));
        });
      }
      draw();
      return h('div', { class: 'card stack' }, h('h2', { text: 'Your Home' }),
        h('p', { class: 'muted small', text: 'The widgets of the ' + role.label.toLowerCase() + ' Home. Switch them on or off and set their order.' }), list,
        h('a', { class: 'btn small', href: '#/home' }, CG.icon('home'), 'See my Home'));
    }
    function profilesPane() {
      var wrap = h('div', { class: 'stack' });
      wrap.appendChild(h('div', { class: 'card stack' }, h('h2', { text: 'Ready-made looks' }),
        h('p', { class: 'muted small', text: 'Applied on top of your current settings; undo with Reset all or by switching back to a saved profile.' }),
        h('div', { class: 'gallery' }, CG.GALLERY.map(function (g) {
          return h('button', { class: 'gcard', type: 'button', onclick: function () { CG.prefs.save(g[2]); CG.toast('Applied "' + g[0] + '".', 'good'); redraw(); } },
            h('b', { text: g[0] }), h('span', { text: g[1] }));
        }))));
      var profs = CG.prefs.profiles(), names = Object.keys(profs);
      var nameIn = h('input', { type: 'text', maxlength: '40', placeholder: 'e.g. Long shift', 'aria-label': 'Profile name' });
      wrap.appendChild(h('div', { class: 'card stack' }, h('h2', { text: 'My saved profiles' }),
        h('p', { class: 'muted small', text: 'Save the current look under a name and switch between looks in one click (up to 12).' }),
        names.length ? h('ul', { class: 'proflist' }, names.map(function (n) {
          return h('li', null, h('b', { text: n }), h('span', { class: 'muted small', text: Object.keys(profs[n]).length + ' settings' }),
            h('button', { class: 'btn small primary', type: 'button', text: 'Use', onclick: function () { CG.prefs.useProfile(n); CG.toast('Switched to "' + n + '".', 'good'); redraw(); } }),
            h('button', { class: 'btn small danger', type: 'button', text: 'Delete', 'aria-label': 'Delete ' + n, onclick: function () { CG.prefs.deleteProfile(n); redraw(); } }));
        })) : h('p', { class: 'muted', text: 'No saved profile yet.' }),
        h('div', { class: 'row' }, h('div', { class: 'grow' }, nameIn), h('button', { class: 'btn', type: 'button', text: 'Save current look', onclick: function () {
          try { var n = CG.prefs.saveProfile(nameIn.value); CG.toast('Saved "' + n + '".', 'good'); redraw(); } catch (e) { CG.toast(e.message, 'warn'); }
        } }))));
      var out = h('textarea', { class: 'json', readonly: true, 'aria-label': 'Your template as JSON' }); out.value = CG.prefs.exportText();
      var inp = h('textarea', { class: 'json', placeholder: 'Paste a template here', 'aria-label': 'Template to import' });
      wrap.appendChild(h('div', { class: 'two' },
        h('div', { class: 'card stack' }, h('h2', { text: 'Share your template' }),
          h('p', { class: 'muted small', text: 'Display choices only: no names, no claims, no access.' }), out,
          h('button', { class: 'btn small', type: 'button', text: 'Copy', onclick: function () {
            out.select();
            (navigator.clipboard ? navigator.clipboard.writeText(out.value) : Promise.reject()).then(function () { CG.toast('Template copied.', 'good'); }, function () { CG.toast('Press Ctrl+C to copy the selected text.', 'info'); });
          } })),
        h('div', { class: 'card stack' }, h('h2', { text: 'Use a template' }), inp,
          h('div', { class: 'row' },
            h('button', { class: 'btn primary small', type: 'button', text: 'Apply template', onclick: function () {
              try { var n = CG.prefs.importText(inp.value); CG.toast('Applied ' + n + ' settings.', 'good'); redraw(); }
              catch (e) { CG.toast(e instanceof SyntaxError ? 'That is not a valid template (JSON).' : e.message, 'bad'); }
            } }),
            h('button', { class: 'btn small', type: 'button', text: 'Reset to the ' + role.label.toLowerCase() + ' template', onclick: function () { CG.prefs.reset(); CG.toast('Back to the ' + role.label.toLowerCase() + ' template.', 'good'); redraw(); } })))));
      return wrap;
    }

    // a file picker dressed as a button: the real input stays in the label, so the keyboard and screen readers reach it
    function pickFile(label, icon, onFile) {
      var input = h('input', { type: 'file', accept: 'image/jpeg,image/png,image/webp,image/gif,image/bmp', class: 'sr-only' });
      input.addEventListener('change', function () { var f = input.files && input.files[0]; input.value = ''; if (f) onFile(f); });
      return h('label', { class: 'btn small primary filebtn' }, CG.icon(icon), h('span', { text: label }), input);
    }
    function bgPane() {
      var cur = CG.prefs.get('bg'), photo = CG.prefs.photo();
      var grid = h('div', { class: 'bggrid', role: 'group', 'aria-label': 'Background' });
      CG.BACKGROUNDS.forEach(function (b) {
        if (b[0] === 'photo' && !photo) return;
        var t = h('button', { class: 'bgtile art-' + b[0], type: 'button', 'aria-pressed': String(cur === b[0]) }, h('span', { class: 'bgname', text: b[1] }));
        if (b[0] === 'photo') t.appendChild(h('img', { src: photo, alt: '' }));
        t.addEventListener('click', function () { save('bg', b[0]); Array.prototype.forEach.call(grid.children, function (x) { x.setAttribute('aria-pressed', String(x === t)); }); });
        grid.appendChild(t);
      });
      var upload = pickFile(photo ? 'Replace my photo' : 'Upload a photo', 'upload', function (file) {
        CG.toast('Preparing your picture…', 'info');
        CG.readImage(file, { max: 1920, quality: 0.82 }).then(function (url) { CG.prefs.savePhoto(url); CG.toast('Your photo is now the background.', 'good'); redraw(); }, function (e) { CG.toast(e.message, 'bad'); });
      });
      return h('div', { class: 'card' }, h('h2', { text: 'Background' }),
        h('p', { class: 'muted small', text: 'Make the console feel like your own desk: a ready-made pattern, or a photo of yours behind the pages. Cards stay solid so claims remain easy to read.' }),
        grid,
        pref('Your own photo', 'A landscape, your team, your city. It is shrunk and kept only in this browser; it is never sent to the server.',
          h('div', { class: 'row' }, upload, photo ? h('button', { class: 'btn small danger', type: 'button', text: 'Remove photo', onclick: function () { CG.prefs.clearPhoto(); redraw(); } }) : null)),
        pref('Veil', 'How much the page colour covers the background, so titles stay readable.', seg('bgDim', [['light', 'Light'], ['medium', 'Medium'], ['strong', 'Strong']])),
        pref('Glass cards', 'Cards let a blurred hint of the background through.', toggle('glass', 'Glass cards')));
    }
    function alertsPane() {
      var desk = toggle('notifyDesktop', 'Desktop notifications');
      desk.addEventListener('change', function () {
        if (!desk.checked) return;
        if (!window.Notification) { desk.checked = false; save('notifyDesktop', false); CG.toast('This browser has no desktop notifications.', 'warn'); return; }
        Notification.requestPermission().then(function (p) { if (p !== 'granted') { desk.checked = false; save('notifyDesktop', false); CG.toast('The browser did not allow notifications. Allow them in the site settings to use this.', 'warn'); } });
      });
      var sounds = seg('notifySound', CG.SOUNDS);
      Array.prototype.forEach.call(sounds.children, function (b, i) { b.addEventListener('click', function () { CG.playSound(CG.SOUNDS[i][0], CG.prefs.get('notifyVolume')); }); });
      var c = h('div', { class: 'card' }, h('h2', { text: 'Notifications' }),
        h('p', { class: 'muted small', text: 'The bell tells you when new claims arrive' + (CG.has('claims.decide') ? ' and when claims are routed to your inbox' : '') + '. It checks every few seconds while you are signed in.' }),
        pref('Notifications', 'Show the alerts and play the sound.', toggle('notify', 'Notifications')));
      if (CG.has('claims.decide')) c.appendChild(pref('My inbox', 'Also alert me when claims are routed to my inbox.', toggle('notifyInbox', 'Inbox alerts')));
      c.appendChild(pref('Sound', 'The Guard chime is ClaimGuard’s own: three rising notes, like the three nodes of the logo’s check. Click one to hear it.', sounds));
      c.appendChild(pref('Volume', 'How loud the sound plays.', seg('notifyVolume', [['low', 'Low'], ['medium', 'Medium'], ['high', 'High']])));
      c.appendChild(pref('Desktop notifications', 'When this tab is in the background, also show a system notification (the browser asks once).', desk));
      c.appendChild(h('div', { class: 'row' },
        h('button', { class: 'btn small', type: 'button', onclick: function () { CG.notify.test(); } }, CG.icon('volume'), 'Play my sound'),
        CG.notify.isDemo() ? h('button', { class: 'btn small primary', type: 'button', onclick: function () { CG.notify.simulate(); } }, CG.icon('bolt'), 'Simulate new claims (demo)') : null));
      return c;
    }
    function mePane() {
      var pic = CG.avatarOf(me.badge_id);
      var upload = pickFile(pic ? 'Change picture' : 'Upload a picture', 'image', function (file) {
        CG.readImage(file, { max: 256, square: true, quality: 0.86, limit: 300000 }).then(function (url) {
          CG.saveAvatar(me.badge_id, url); CG.toast('Profile picture saved.', 'good'); redraw();
        }, function (e) { CG.toast(e.message, 'bad'); });
      });
      return h('div', { class: 'card' }, h('h2', { text: 'Profile picture' }),
        h('div', { class: 'mepic' }, CG.avatar(me.name, me.badge_id, 'xl'),
          h('div', { class: 'stack' }, h('b', { text: me.name }), h('span', { class: 'muted', text: me.badge_id + ' · ' + role.label }), CG.levelBadge(me.level),
            h('div', { class: 'row' }, upload, pic ? h('button', { class: 'btn small danger', type: 'button', text: 'Remove', onclick: function () { CG.clearAvatar(me.badge_id); CG.toast('Back to your initials.', 'good'); redraw(); } }) : null))),
        h('p', { class: 'muted small', text: 'The picture is cropped to a square and shown in the menu and on your card on the sign-in page. It stays in this browser: colleagues on other computers still see your initials, and your clearance ring keeps your level colour.' }));
    }

    // Accessibility: colour vision, reading and focus aids, with one-click starting points. Every choice is also a separate setting.
    function a11yPane() {
      var presets = h('div', { class: 'a11y-presets' }, CG.A11Y_PRESETS.map(function (pr) {
        return h('button', { class: 'a11y-preset', type: 'button', onclick: function () { CG.prefs.save(pr[2]); CG.toast(pr[0] + ': applied. You can still change each setting below.', 'good'); redraw(); } },
          h('b', { text: pr[0] }), h('span', { text: pr[1] }));
      }));
      var clear = h('button', { class: 'btn small', type: 'button', text: 'Clear accessibility settings', onclick: function () {
        var base = CG.prefs.preset(), c = {};
        CG.A11Y_KEYS.forEach(function (k) { c[k] = base[k]; });
        CG.prefs.save(c); CG.toast('Accessibility settings back to your template.', 'good'); redraw();
      } });
      var sample = h('div', { class: 'a11y-sample', 'aria-hidden': 'true' }, CG.chip('FAIL'), CG.chip('high'), CG.chip('medium'), CG.chip('UNABLE_TO_ASSESS'), CG.chip('PASS'), CG.chip('NOT_APPLICABLE'));
      return h('div', { class: 'stack' },
        h('div', { class: 'card stack' }, h('h2', { text: 'Accessibility' }),
          h('p', { class: 'muted', text: 'Settings that make the console easier to see and to read. Start from what fits you, then adjust. They are kept for your badge in this browser, like every other setting here.' }),
          presets, h('div', { class: 'row' }, clear)),
        h('div', { class: 'card' }, h('h2', { text: 'Colour vision' }),
          pref('Status colours', 'Colours for fail, high, medium and pass that people with this kind of colour blindness can tell apart. Text keeps WCAG AA contrast.',
            seg('vision', [['standard', 'Standard'], ['redgreen', 'Red-green'], ['blueyellow', 'Blue-yellow'], ['mono', 'Greyscale']])),
          pref('Shapes on statuses', 'Adds a shape to every status (▲ high, ◆ medium, ✕ fail, ? unable to assess, ✓ pass) and a different edge style on findings, so colour is never the only signal. Always on with a colour-vision mode.',
            toggle('shapes', 'Shapes on statuses')),
          h('div', { class: 'pref' }, h('div', { class: 'pl' }, h('b', { text: 'How statuses look now' }), h('span', { text: 'Updates as you choose.' })), sample)),
        h('div', { class: 'card' }, h('h2', { text: 'Reading' }),
          pref('Typeface', '“Easy to read” uses a dyslexia-friendly or low-vision typeface if one is installed on this computer (OpenDyslexic, Lexend, Atkinson Hyperlegible), otherwise Verdana. Nothing is downloaded.',
            seg('font', [['system', 'Standard'], ['readable', 'Easy to read'], ['humanist', 'Humanist'], ['serif', 'Serif'], ['mono', 'Mono']])),
          pref('Text size', 'Larger text everywhere.', seg('text', [['s', 'S'], ['m', 'M'], ['l', 'L'], ['xl', 'XL']])),
          pref('Text spacing', 'More space between letters, words and lines. "Wide" follows the WCAG text-spacing values.', seg('spacing', [['normal', 'Normal'], ['relaxed', 'Relaxed'], ['wide', 'Wide']])),
          pref('Reading ruler', 'A soft band follows your pointer so your eye keeps its line in long text and tables.', toggle('ruler', 'Reading ruler'))),
        h('div', { class: 'card' }, h('h2', { text: 'Seeing and moving around' }),
          pref('Contrast', 'High contrast makes text darker and borders stronger.', seg('contrast', [['normal', 'Normal'], ['high', 'High']])),
          pref('Strong focus ring', 'A thick, two-colour ring around whatever the keyboard is on.', toggle('bigFocus', 'Strong focus ring')),
          pref('Underline links', 'Underline every link, not only on hover.', toggle('underline', 'Underline links')),
          pref('Animation', 'Turn every animation and movement off.', seg('motion', [['system', 'Follow the system'], ['reduce', 'Off']]))));
    }

    var TABS = [['look', 'Look', 'palette', lookPane], ['a11y', 'Accessibility', 'eye', a11yPane], ['bg', 'Background', 'image', bgPane], ['me', 'Profile picture', 'users', mePane], ['alerts', 'Notifications', 'bell', alertsPane],
      ['nav', 'Layout', 'grid', navPane], ['claim', 'Claim page', 'shield', claimPane],
      ['lists', 'Lists', 'list', listsPane], ['home', 'Home', 'home', homePane], ['profiles', 'Profiles and sharing', 'star', profilesPane]];
    if (!TABS.some(function (t) { return t[0] === tab; })) tab = 'look';
    root.appendChild(CG.pageHead({ title: 'Customize your console', sub: 'Your template starts from the ' + role.label.toLowerCase() + ' template. Changes apply at once and are kept in this browser for badge ' + me.badge_id + '.',
      actions: [status, h('button', { class: 'btn small', type: 'button', text: 'Reset all', onclick: function () { CG.prefs.reset(); CG.toast('Back to the ' + role.label.toLowerCase() + ' template.', 'good'); redraw(); } })] }));
    paintStatus();
    var pane = h('div', { role: 'tabpanel', id: 'cg-tabpanel' });
    var bar = h('div', { class: 'tabs', role: 'tablist', 'aria-label': 'Settings' });
    TABS.forEach(function (t) {
      var b = h('button', { type: 'button', role: 'tab', 'data-tab': t[0], 'aria-selected': String(t[0] === tab), 'aria-controls': 'cg-tabpanel', tabindex: t[0] === tab ? '0' : '-1' }, CG.icon(t[2]), t[1]);
      b.addEventListener('click', function () {
        CG.state.customTab = t[0]; tab = t[0];
        Array.prototype.forEach.call(bar.children, function (x) { x.setAttribute('aria-selected', String(x === b)); x.setAttribute('tabindex', x === b ? '0' : '-1'); });
        CG.clear(pane); pane.appendChild(t[3]());
      });
      b.addEventListener('keydown', function (e) {
        var i = TABS.indexOf(t), j = e.key === 'ArrowRight' ? (i + 1) % TABS.length : e.key === 'ArrowLeft' ? (i + TABS.length - 1) % TABS.length : -1;
        if (j >= 0) { e.preventDefault(); bar.children[j].click(); bar.children[j].focus(); }
      });
      bar.appendChild(b);
    });
    pane.appendChild(TABS.filter(function (t) { return t[0] === tab; })[0][3]());

    // live preview: a miniature console and a sample finding, drawn with the real classes so every choice shows at once
    var preview = h('div', { class: 'card preview-pane stack' }, h('h2', { text: 'Preview' }),
      h('div', { class: 'mini', 'aria-hidden': 'true' }, h('div', { class: 'ms' }, h('i'), h('i', { class: 'a' }), h('i'), h('i')), h('div', { class: 'mm' }, h('i'), h('i'), h('i', { class: 'b' }))),
      h('div', { class: 'card finding flag-high' },
        h('div', { class: 'fhead' }, h('div', null, h('div', { class: 'row tight' }, h('span', { class: 'rid', text: 'R008' }), CG.chip('FAIL'), CG.chip('high')), h('div', { class: 'rtitle', text: CG.ruleName('R008') })), h('span', { class: 'lines', text: 'Line L1' })),
        h('div', { class: 'fbody' }, h('p', { class: 'explain', text: 'Sample text: the imaging line has no authorization reference.' }),
          h('div', { class: 'row tight' }, h('button', { class: 'btn primary small', type: 'button', text: 'Confirm the issue' }), h('a', { href: '#/customize', text: 'a link' })))),
      h('div', { class: 'row' }, CG.levelBadge(me.level), h('span', { class: 'muted small', text: 'Your level badge keeps its colour.' })),
      h('p', { class: 'muted small', text: 'Sample only, not a real claim.' }));
    root.appendChild(bar);
    root.appendChild(h('div', { class: 'prefgrid' }, h('div', { class: 'col' }, pane), h('div', { class: 'col' }, preview)));
  };

  // ---- Brand: the identity sheet (docs/brand), drawn from the same logo files the console uses; colours come from style.css classes
  var PALETTE = [
    ['navy', 'Primary', 'Administration Navy', '#15294C', 'Sidebar, primary buttons, the shield'],
    ['deep', 'Depth', 'Deep Navy', '#0E1D38', 'The shield rim, dark surfaces'],
    ['steel', 'Active', 'Steel Blue', '#243E70', 'Open menu item, hover on navy'],
    ['ink', 'Text', 'Ink', '#13233F', 'Titles and body text on light'],
    ['slate', 'Secondary', 'Slate', '#5B6578', 'Labels, table headers, hints'],
    ['mist', 'On navy', 'Mist', '#9FB0CC', 'Secondary text on navy'],
    ['line', 'Lines', 'Line Grey', '#E2E6EC', 'Card borders, table rules'],
    ['paper', 'Surface', 'Paper', '#F6F7F9', 'Page background (cards are white)'],
    ['green', 'Success', 'Pass Green', '#187A4B', 'Passed rule, done'],
    ['amber', 'Warning', 'Caution Amber', '#9A5B05', 'Unable to assess, medium severity'],
    ['red', 'Error', 'Fail Red', '#B42318', 'Failed rule, high severity']
  ];
  var CONTRAST = [['Administration Navy #15294C', '14.4', '-', 'On light'], ['Ink #13233F', '15.7', '-', 'On light'], ['Steel Blue #243E70', '10.5', '-', 'On light'],
    ['Slate #5B6578', '5.9', '2.5', 'On light'], ['Mist #9FB0CC', '2.2', '6.6', 'On navy only'], ['White #FFFFFF', '-', '14.4', 'On navy (and 10.5 on Steel Blue)'],
    ['Pass Green #187A4B', '5.4', '2.7', 'On light'], ['Caution Amber #9A5B05', '5.4', '2.7', 'On light'], ['Fail Red #B42318', '6.6', '2.2', 'On light']];
  CG.views.brand = function (root) {
    root.appendChild(h('section', { class: 'bs-hero' },
      h('span', { class: 'bs-eyebrow', text: 'Brand identity · v2.0' }),
      h('img', { src: 'logo-dark.svg', alt: 'ClaimGuard Claims administration', class: 'bs-heroimg' }),
      h('p', { text: 'A shield that holds a claim document with a check: every claim is protected and verified before it reaches the payer. Navy and white, flat and calm, for people who read claims all day.' })));
    root.appendChild(h('div', { class: 'two' },
      h('figure', { class: 'bs-panel light' }, h('img', { src: 'logo.svg', alt: 'Logo on a light background' }), h('figcaption', { text: 'logo.svg · on white and light surfaces' })),
      h('figure', { class: 'bs-panel dark' }, h('img', { src: 'logo-dark.svg', alt: 'Logo on navy' }), h('figcaption', { text: 'logo-dark.svg · on Administration Navy' }))));
    root.appendChild(h('div', { class: 'card' }, h('h2', { text: 'The mark' }),
      h('div', { class: 'bs-sizes' }, [[128, 'store, splash'], [80, 'sign-in'], [32, 'tab'], [16, 'favicon']].map(function (s) {
        return h('figure', null, h('img', { src: 'icon.svg', alt: '', width: String(s[0]), height: String(s[0]) }), h('figcaption', { text: s[0] + ' px · ' + s[1] }));
      }).concat([h('figure', null, h('div', { class: 'onnavy' }, h('img', { src: 'icon-light.svg', alt: '', width: '36', height: '36' })), h('figcaption', { text: 'icon-light.svg · sidebar' }))])),
      h('div', { class: 'two' },
        h('ul', { class: 'plain' }, h('li', null, h('b', { text: 'Shield' }), ' (navy, with a Deep Navy rim): protection of every claim.'),
          h('li', null, h('b', { text: 'Document' }), ' (white, folded corner): the claim itself.'),
          h('li', null, h('b', { text: 'Check' }), ': the pre-validation. The engine checks, a person decides.')),
        h('ul', { class: 'plain' }, h('li', { text: 'Navy mark on light surfaces; the white mark (icon-light.svg) on navy.' }), h('li', { text: 'Do not recolour, stretch or add shadows to the mark.' }),
          h('li', { text: 'Below 120 px wide, use the mark alone.' })))));
    root.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Name and line' }),
      h('p', { class: 'muted small', text: '"ClaimGuard" in bold; "Claims administration" in small capitals, widely spaced. The tagline stays available for presentations.' }),
      h('div', { class: 'bs-tag' }, 'Smarter checks, ', h('span', { text: 'safer claims' }))));
    root.appendChild(h('div', { class: 'sectionhead' }, h('h2', { text: 'Colour palette' })));
    root.appendChild(h('div', { class: 'bs-palette' }, PALETTE.map(function (c) {
      return h('div', { class: 'bs-sw' }, h('div', { class: 'bs-chip bs-' + c[0], text: c[1] }),
        h('div', { class: 'bs-meta' }, h('b', { text: c[2] }), h('code', { text: c[3] }), h('span', { text: c[4] })));
    })));
    root.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Text colours and contrast' }),
      h('p', { class: 'muted small', text: 'WCAG ratios; 4.5 is the minimum for body text. Mist is for secondary text on navy only.' }),
      CG.table([{ label: 'Colour', get: function (r) { return r[0]; } }, { label: 'On white', num: true, get: function (r) { return r[1]; } },
        { label: 'On navy', num: true, get: function (r) { return r[2]; } }, { label: 'Use for text', get: function (r) { return h('b', { text: r[3] }); } }], CONTRAST)));
    root.appendChild(h('div', { class: 'card' }, h('h2', { text: 'Typography' }),
      h('div', { class: 'bs-type', text: 'ClaimGuard' }),
      h('p', { class: 'muted', text: 'IBM Plex Sans when it is installed, otherwise Segoe UI: 700 for the name, 600 to 650 for titles, 400 to 500 for reading. Nothing is downloaded.' }),
      h('code', { text: 'CG-4134DC69B4DC · R013 · Cascadia Mono for identifiers' })));
  };
})();
