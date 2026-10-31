/* ClaimGuard console: the reviewer's work (inbox, a claim under review, the browse list). */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;

  var ACTIONS = {
    confirm_issue: ['Confirm the issue', 'The finding is right. The claim needs correction before it can go on.'],
    dismiss_with_reason: ['Dismiss with a reason', 'The finding does not apply here. Your reason is kept in the audit log.'],
    request_information: ['Request information', 'You cannot judge it yet. Ask the submitter for documents or a clarification.'],
    mark_corrected_for_recheck: ['Mark corrected for recheck', 'The submitter fixed the problem. Send it back through the rule engine.']
  };
  var STAGES = { countersign: 'Waiting for a second senior (countersign)', tiebreak: 'Seniors disagreed: final decision by a third senior' };

  // A button that asks for a second click (no browser dialog): the first click arms it for five seconds, the second runs it.
  function twoStep(label, armedLabel, run) {
    var armed = false, timer = null;
    var b = h('button', { class: 'btn', type: 'button', text: label });
    function reset() { armed = false; b.textContent = label; b.className = 'btn'; }
    b.addEventListener('click', function () {
      if (!armed) { armed = true; b.textContent = armedLabel; b.className = 'btn primary'; timer = setTimeout(reset, 5000); return; }
      clearTimeout(timer); reset(); run(b);
    });
    return b;
  }

  function canConfirm(f) { return f.allowed_actions && f.allowed_actions.indexOf('confirm_issue') >= 0; }

  var hotkeyOff = null;
  function hotkey(key, fn) {
    if (hotkeyOff) hotkeyOff();
    function on(e) {
      if (e.ctrlKey || e.metaKey || e.altKey || e.key !== key) return;
      var tag = e.target && e.target.tagName;
      if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return;
      e.preventDefault(); fn();
    }
    function off() { document.removeEventListener('keydown', on); if (hotkeyOff === off) hotkeyOff = null; }
    document.addEventListener('keydown', on);
    hotkeyOff = off;
    (CG.state.streams = CG.state.streams || []).push({ close: off });
  }

  // After the last finding of a claim, open the next claim at once instead of going back to the inbox: one click less per claim.
  function goNext(back) {
    CG.api('GET', '/work/inbox').then(function (d) {
      if (d.claims && d.claims.length) { location.hash = '#/work/' + encodeURIComponent(d.claims[0].claim_id); return null; }
      return CG.api('POST', '/work/next').then(function (n) { location.hash = n && n.claim ? '#/work/' + encodeURIComponent(n.claim.claim_id) : back; });
    }).then(null, function () { location.hash = back; });
  }

  // Confirming agrees with the engine, so it needs no typed reason (the server records a fixed one). Only overriding the engine needs words.
  function confirmNow(ctx, f) {
    return CG.api('POST', ctx.decisionPath(f.rule_id), { action: 'confirm_issue' });
  }

  function confirmAll(list, ctx, refresh) {
    var n = list.length, busy = false, armedAt = 0;
    function run(b) {
      if (busy) return;
      busy = true; if (b) b.disabled = true;
      list.reduce(function (p, f) { return p.then(function () { return confirmNow(ctx, f); }); }, Promise.resolve()).then(function () {
        CG.toast('Confirmed ' + n + ' findings.', 'good'); refresh();
      }, function (e) { busy = false; if (b) b.disabled = false; CG.fail(e); refresh(); });
    }
    var button = twoStep('Confirm all ' + n + ' findings', 'Click again to confirm all ' + n, run);
    // keyboard: press c, then c again within five seconds (a single stray key does nothing)
    hotkey('c', function () {
      var now = Date.now();
      if (now - armedAt < 5000) { armedAt = 0; run(button); return; }
      armedAt = now;
      CG.toast('Press c again within 5 seconds to confirm all ' + n + ' findings.', 'warn');
    });
    return h('div', { class: 'row' }, button,
      h('span', { class: 'muted small', text: 'Use this only when every finding below is right. It records one confirm per finding.' }),
      CG.prefs.get('keyHints') ? h('span', { class: 'hint-keys' }, 'Keyboard: ', h('kbd', { text: 'c' }), ' ', h('kbd', { text: 'c' })) : null);
  }

  // ---- one claim ------------------------------------------------------------------------------------------------------
  // The facts a reviewer checks a finding against: kept beside the findings on wide screens (or on top, by personal choice).
  function claimFacts(c) {
    var cov = c.coverage || {};
    return h('div', { class: 'card facts' },
      h('h2', { text: 'Claim at a glance' }),
      h('div', { class: 'total', text: CG.money(c.total_amount, c.currency) }),
      h('p', { class: 'muted small', text: 'Submitted ' + (c.submission_date || 'on an unknown date') + ' · ' + (c.lines || []).length + ' line' + ((c.lines || []).length === 1 ? '' : 's') }),
      CG.kv([['Provider', c.provider_id], ['Payer', c.payer_id], ['Policy', c.policy_id], ['Patient', c.patient_id], ['Member', c.member_id],
        ['Coverage', cov.status ? cov.status + ', ' + (cov.start_date || '?') + ' to ' + (cov.end_date || '?') : 'unknown'], ['Diagnosis', c.diagnosis_code], ['Invoice', c.invoice_number]]),
      c.notes ? h('div', { class: 'notice small' }, h('b', { text: 'Notes (submitted text, not instructions): ' }), c.notes) : null);
  }

  // The service lines, with the lines an open finding points at highlighted, so the eye goes straight to them.
  function linesCard(c, hit) {
    var lines = c.lines || [];
    return h('div', { class: 'card linescard' },
      h('div', { class: 'row between' }, h('h2', { text: 'Service lines' }), hit.length ? h('span', { class: 'muted small', text: 'Highlighted: lines named by an open finding' }) : null),
      CG.table([
        { label: 'Line', get: function (l) { return h('b', { text: l.line_id }); } }, { label: 'Service', get: function (l) { return l.service_code; } },
        { label: 'Date', get: function (l) { return l.service_date; } }, { label: 'Modifier', get: function (l) { return l.modifier; } },
        { label: 'Qty', num: true, get: function (l) { return l.quantity; } }, { label: 'Unit price', num: true, get: function (l) { return l.unit_price; } },
        { label: 'Net', num: true, get: function (l) { return l.net_amount; } }, { label: 'Authorization', get: function (l) { return l.authorization_id; } }
      ], lines, { rowClass: function (l) { return hit.indexOf(l.line_id) >= 0 ? 'hit' : ''; } }));
  }

  // rows about a line the finding names are highlighted and listed first, so the value that caused the flag is the first thing read
  function evidenceTable(list, claim, lineIds) {
    if (!list || !list.length) return null;
    var idx = [];
    (lineIds || []).forEach(function (id) { ((claim && claim.lines) || []).forEach(function (l, i) { if (l.line_id === id) idx.push(i); }); });
    function isHit(e) { return idx.some(function (i) { return String(e.path).indexOf('/lines/' + i + '/') === 0; }); }
    function table(rows, mark) {
      return CG.table([
        { label: 'Where', get: function (e) { return h('span', null, h('span', { text: CG.pathLabel(e.path, claim) }), h('span', { class: 'path', text: e.path })); } },
        { label: 'Value seen', get: function (e) { return h('span', { class: 'val', text: CG.fmtValue(e.value) }); } }], rows, { rowClass: function (e) { return mark && isHit(e) ? 'hit' : ''; } });
    }
    var hits = idx.length ? list.filter(isHit) : [], rest = idx.length ? list.filter(function (e) { return !isHit(e); }) : [];
    // the values about the named line come first and are highlighted; the rest of the evidence is one click away so the decision is not pushed down
    if (!hits.length || !rest.length) return h('div', { class: 'evidence' }, table(list, hits.length > 0));
    return h('div', { class: 'evidence' }, table(hits, true),
      h('details', { class: 'evidence-rest' }, h('summary', { class: 'small', text: rest.length + ' other value' + (rest.length > 1 ? 's' : '') + ' the engine looked at' }), table(rest, false)));
  }

  function decisionForm(finding, ctx, refresh) {
    var chosen = null, busy = false;
    var reason = h('textarea', { maxlength: '1000', placeholder: 'Why? Write what you checked and what you found.', 'aria-label': 'Reason' });
    var reasonLabel = h('label', { text: 'Reason (kept in the audit log; optional when you confirm)' });
    var send = h('button', { class: 'btn primary', type: 'button', disabled: true, text: 'Record decision' });
    var boxes = [];
    var choices = h('div', { class: 'choices', role: 'radiogroup' }, finding.allowed_actions.map(function (a) {
      var radio = h('input', { type: 'radio', name: 'act-' + ctx.id + '-' + finding.rule_id, value: a });
      var box = h('label', { class: 'choice' }, radio, h('span', null, h('b', { text: (ACTIONS[a] || [a])[0] }), h('span', { class: 'muted small', text: (ACTIONS[a] || ['', ''])[1] })));
      radio.addEventListener('change', function () {
        chosen = a; boxes.forEach(function (b) { b.className = 'choice' + (b === box ? ' on' : ''); }); sync();
      });
      boxes.push(box);
      return box;
    }));
    function sync() {
      var needs = chosen && chosen !== 'confirm_issue';
      reasonLabel.textContent = needs ? 'Reason (required, kept in the audit log)' : 'Reason (kept in the audit log; optional when you confirm)';
      send.disabled = busy || !chosen || (needs && !reason.value.trim());
    }
    reason.addEventListener('input', sync);
    send.addEventListener('click', function () {
      busy = true; sync();
      CG.api('POST', ctx.decisionPath(finding.rule_id), { action: chosen, reason: reason.value.trim() }).then(function () {
        CG.toast('Decision recorded for ' + finding.rule_id + '.', 'good'); refresh();
      }, function (e) { busy = false; sync(); CG.fail(e); });
    });
    var quick = canConfirm(finding) ? h('div', { class: 'row' },
      h('button', { class: 'btn primary', type: 'button', onclick: function () {
        if (busy) return;
        busy = true; sync();
        confirmNow(ctx, finding).then(function () { CG.toast('Confirmed ' + finding.rule_id + '.', 'good'); refresh(); }, function (e) { busy = false; sync(); CG.fail(e); });
      } }, CG.icon('check'), 'Confirm the issue'),
      h('span', { class: 'muted small', text: 'One click when the finding is right. No reason needed.' })) : null;
    var full = h('div', { class: 'stack' }, choices, h('div', { class: 'field' }, reasonLabel, reason), send);
    if (!quick) return h('div', { class: 'decide stack' }, h('h3', { text: 'Your decision' }), full);
    // the other actions override the engine and need a reason: one click away, so the common case stays one click
    var more = h('details', null, h('summary', { class: 'small', text: 'Another action: dismiss, request information, or mark corrected' }), full);
    return h('div', { class: 'decide stack' }, h('h3', { text: 'Your decision' }), quick, more);
  }

  function findingCard(f, ctx, refresh, claim) {
    var flagged = f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS';
    var kind = !flagged ? 'flag-pass' : f.severity === 'high' ? 'flag-high' : 'flag-medium';
    var open = f.allowed_actions && f.allowed_actions.length;
    var card = h('article', { class: 'card finding ' + kind + (flagged && !open ? ' settled' : ''), 'aria-label': f.rule_id + ' ' + CG.ruleName(f.rule_id) },
      h('div', { class: 'fhead' },
        h('div', null,
          h('div', { class: 'row tight' }, h('span', { class: 'rid', text: f.rule_id }), CG.chip(f.status), flagged ? CG.chip(f.severity) : null, f.review_state ? CG.chip(f.review_state) : null),
          h('div', { class: 'rtitle', text: CG.ruleName(f.rule_id) || f.rule_id })),
        f.affected_line_ids && f.affected_line_ids.length ? h('span', { class: 'lines', text: (f.affected_line_ids.length > 1 ? 'Lines ' : 'Line ') + f.affected_line_ids.join(', ') }) : null));
    var body = h('div', { class: 'fbody' }, f.explanation ? h('p', { class: 'explain', text: f.explanation }) : null);
    if (flagged && ctx.why && ctx.why[f.rule_id]) body.appendChild(h('p', { class: 'why' }, h('b', { text: 'Why it was flagged: ' }), ctx.why[f.rule_id]));
    card.appendChild(body);
    if (!flagged) return card;
    if (CG.prefs.get('showAi') && f.ai_explanation && f.ai_explanation.text !== f.explanation) {      // the built-in template restates the line above: show the box only when it adds something
      body.appendChild(h('div', { class: 'ai-box' },
        h('div', { class: 'row tight' }, CG.chip('AI draft', 'ai'), h('span', { class: 'muted small', text: 'Source: ' + f.ai_explanation.source + '. The rule engine decided the status; this text only helps you read it. Check it against the evidence.' })),
        h('p', { text: f.ai_explanation.text })));
    }
    if (f.corrective_action && CG.prefs.get('showTodo')) body.appendChild(h('div', { class: 'todo-box' }, h('b', { text: 'What the rulebook says to do' }), f.corrective_action));
    var ev = evidenceTable(f.evidence, claim, f.affected_line_ids);
    if (ev) {
      var det = h('details', null, h('summary', { text: 'Evidence the engine used (' + f.evidence.length + ')' }), ev);
      if (CG.prefs.get('evidenceOpen') && f.evidence.length <= 6) det.open = true;      // a long list stays one click away so the decision is not pushed down
      body.appendChild(det);
    }
    if (open) body.appendChild(decisionForm(f, ctx, refresh));
    else if (f.review_state === 'resolved') body.appendChild(h('p', { class: 'muted small' }, CG.icon('check'), ' Settled.'));
    else if (!CG.has('claims.decide')) body.appendChild(h('p', { class: 'muted small readonly' }, CG.icon('lock'), h('span', { text: 'Read-only: reviewers decide this finding.' })));
    else body.appendChild(h('p', { class: 'muted small', text: 'You cannot decide this finding with your clearance (hidden actions are not offered).' }));
    return card;
  }

  var RULE_CHOICES = ['R001', 'R002', 'R003', 'R004', 'R005', 'R006', 'R007', 'R008', 'R009', 'R010', 'R011', 'R012', 'R013', 'R014', 'R015'];

  function greenActions(view, ctx, refresh) {
    var category = null, note = null;
    function send(action) {
      var body = { action: action };
      if (action === 'escalate' && view.in_batch) {
        if (!category.value) { CG.toast('Choose the rule that should have flagged this claim, or Other.', 'warn'); return; }
        body.category = category.value;
        if (note.value.trim()) body.note = note.value.trim();
      }
      CG.api('POST', '/work/claims/' + encodeURIComponent(ctx.id) + '/verify', body).then(function () {
        CG.toast(action === 'verify_clear' ? 'Marked pre-validation clear (not a payer approval).' : (view.in_batch ? 'Flagged. The whole batch is held until an administrator decides.' : 'Sent to a senior reviewer.'), 'good'); refresh();
      }, CG.fail);
    }
    var flagBox = null;
    if (view.in_batch) {
      category = h('select', { 'aria-label': 'Which rule should have flagged this claim' }, h('option', { value: '', text: 'Which rule should have flagged it?' }),
        RULE_CHOICES.map(function (r) { return h('option', { value: r, text: r }); }), h('option', { value: 'other', text: 'Other / none of them' }));
      note = h('input', { type: 'text', maxlength: '200', placeholder: 'Optional note (no patient identifiers)' });
      flagBox = h('div', { class: 'stack' },
        h('div', { class: 'notice warn', text: 'This claim was picked at random to check how the system cleared its batch. If you find something the engine missed, flag it: the whole batch is held at once and an administrator decides what happens to it.' }),
        h('div', { class: 'row' }, category, note));
    }
    return h('div', { class: 'card flag-pass stack green-card' },
      h('h2', null, CG.icon('check'), 'No rule flagged this claim'),
      h('p', { text: 'All checks completed with no finding. This is a pre-validation clearance only: it is not payer approval and promises no payment. A person still looks at every claim: confirm it, or send it up if something looks wrong to you.' }),
      flagBox,
      h('div', { class: 'row' },
        h('button', { class: 'btn primary', type: 'button', text: 'Verify: pre-validation clear', onclick: function () { send('verify_clear'); } }),
        h('button', { class: 'btn', type: 'button', text: view.in_batch ? 'Flag: the engine missed something' : 'Escalate to a senior', onclick: function () { send('escalate'); } })));
  }

  function unmaskBox(ctx, redraw) {
    var reason = h('input', { type: 'text', maxlength: '500', placeholder: 'Why do you need the real identifiers?' });
    var open = h('div');
    var go = h('button', { class: 'btn small', type: 'button', text: 'Show real identifiers', onclick: function () {
      if (!reason.value.trim()) { CG.toast('Write the reason first. It goes in the audit log.', 'warn'); return; }
      CG.api('POST', '/claims/' + encodeURIComponent(ctx.id) + '/unmask', { reason: reason.value.trim() }).then(function (v) { redraw(v); }, CG.fail);
    } });
    open.appendChild(h('div', { class: 'row' }, h('div', { class: 'grow' }, reason), go));
    return h('div', { class: 'notice' }, h('b', { text: 'Identifiers are masked. ' }), h('span', { class: 'muted', text: 'You may unmask them for a stated reason; the act is logged.' }), open);
  }

  // ctx: { id, mode: 'work' | 'browse', decisionPath(rule), reload() -> Promise<view> }
  CG.renderClaim = function (root, view, ctx) {
    CG.clear(root);
    var back = ctx.mode === 'work' ? '#/inbox' : '#/claims';
    var flagged = view.findings.filter(function (f) { return f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS'; });
    function refresh() {
      ctx.reload().then(function (v) {
        if (!v) {
          CG.toast(ctx.mode === 'work' ? 'Claim finished. Opening the next one.' : 'This claim is finished and has left your inbox.', 'good');
          if (ctx.mode === 'work') goNext(back); else location.hash = back;
          return;
        }
        CG.renderClaim(root, v, ctx);
      }, function (e) { CG.fail(e); });
    }
    var lease = null;
    if (view.leased_until) {
      var leaseText = h('span');
      lease = h('span', { class: 'lease', title: 'Staying on this page keeps the lease alive. If it lapses the claim goes back to the pool.', role: 'timer', 'aria-live': 'off' }, CG.icon('clock'), leaseText);
      var tick = function () {
        var left = view.leased_until - Date.now() / 1000;
        leaseText.textContent = left > 0 ? CG.fmtDuration(left) + ' left on your lease' : 'Your lease has run out';
        lease.className = 'lease' + (left <= 0 ? ' out' : left < 300 ? ' warn' : '');
      };
      tick(); CG.every(1000, tick);
    }
    // patient history: the card sits under the lines, and a button next to the claim number jumps there and loads it, so it is easy to find
    ctx.why = ctx.mode === 'work' && CG.copilotWhy ? CG.copilotWhy(view.copilot) : {};
    var hist = ctx.mode === 'work' ? historyCard(ctx) : null;
    var histJump = hist ? h('button', { class: 'btn histjump', type: 'button', onclick: function () {
      var col = hist.parentNode;              // bring the card to the top of the claim, right under this button, then load it
      if (col && col.firstChild !== hist) col.insertBefore(hist, col.firstChild);
      hist.scrollIntoView({ behavior: 'smooth', block: 'start' }); hist.load();
    } },
      CG.icon('list'), 'Patient history') : null;
    root.appendChild(CG.pageHead({ cls: 'claimhead', title: ctx.id,
      crumbs: [[ctx.mode === 'work' ? 'My inbox' : 'All claims', back], ['Claim']],
      chips: [view.lane ? CG.chip(view.lane === 'green' ? 'Clean (green lane)' : 'Lane ' + view.lane, view.lane === 'green' ? 'green' : view.lane) : null,
        view.score !== undefined ? h('span', { class: 'muted small', text: 'priority score ' + view.score }) : null,
        view.version ? h('span', { class: 'muted small', text: 'version ' + view.version }) : null],
      actions: [histJump, lease] }));

    // progress: what is left to do on this claim, in numbers
    var readOnly = !CG.has('claims.decide');
    var openCount = flagged.filter(function (f) { return readOnly ? f.review_state !== 'resolved' : (f.allowed_actions && f.allowed_actions.length); }).length;
    var others = view.findings.length - flagged.length;
    if (flagged.length) {
      var done = flagged.length - openCount, bar = h('div', { class: 'bar', role: 'progressbar', 'aria-valuemin': '0', 'aria-valuemax': String(flagged.length), 'aria-valuenow': String(done), 'aria-label': 'Findings decided' }, h('i'));
      bar.firstChild.style.width = (100 * done / flagged.length) + '%';
      root.appendChild(h('div', { class: 'progress' },
        h('div', { class: 'stats' },
          h('span', { class: 'stat todo' }, h('b', { text: String(openCount) }), readOnly ? (openCount === 1 ? 'finding still open (read-only)' : 'findings still open (read-only)') : (openCount === 1 ? 'finding for you to decide' : 'findings for you to decide')),
          h('span', { class: 'stat done' }, h('b', { text: String(done) }), readOnly ? 'settled' : 'settled or with someone else'),
          h('span', { class: 'stat' }, h('b', { text: String(others) }), 'rules passed or not applicable')), bar));
    }
    if (!CG.has('claims.decide')) root.appendChild(h('div', { class: 'notice info readonly' }, CG.icon('lock'), h('span', { text: 'Read-only view. You can read every finding and its evidence; reviewers record the decisions.' })));
    if (view.signoff_stage && STAGES[view.signoff_stage]) root.appendChild(h('div', { class: 'notice warn', text: STAGES[view.signoff_stage] }));
    if (view.escalated) root.appendChild(h('div', { class: 'notice warn', text: 'This claim was escalated.' }));
    if (ctx.mode === 'browse' && view.allowed_actions && view.allowed_actions.indexOf('unmask') >= 0) {
      root.appendChild(unmaskBox(ctx, function (v) { CG.renderClaim(root, v, ctx); }));
    }

    if (view.corrected_from) {
      var cf = view.corrected_from;
      root.appendChild(h('div', { class: 'notice info' }, h('b', { text: 'Corrected version. ' }), 'Version ' + cf.version + ' was corrected by ' + cf.by + ': ' +
        (cf.changes || []).map(function (c) { return c.path + ' ' + CG.fmtValue(c.from) + ' to ' + CG.fmtValue(c.to); }).join('; ') + '. Reason: ' + cf.reason));
    }
    var fix = null;
    if (ctx.mode === 'work' && view.can_correct && CG.fixPanel) fix = CG.fixPanel(view, ctx, refresh);
    if (ctx.mode === 'work' && view.copilot && CG.copilotPanel) root.appendChild(CG.copilotPanel(view.copilot, fix ? fix.tryEdits : null));
    if (ctx.mode === 'work' && view.info_request && CG.infoRequestCard) root.appendChild(CG.infoRequestCard(view, ctx, refresh));
    var advFlagged = (view.advisory || []).filter(function (f) { return f.status === 'FAIL'; });
    if (ctx.mode === 'work' && advFlagged.length) {
      root.appendChild(h('div', { class: 'notice warn' }, h('b', { text: 'Compared with this patient’s earlier claims: ' }),
        advFlagged.map(function (f) { return f.rule_id + ' (' + f.explanation + ')'; }).join(' '), ' These are advisory checks: they inform your decision and change nothing.'));
    }
    // two columns: what to decide on the left, the facts to decide it against on the right (stacked on narrow screens or by choice)
    var mainCol = h('div', { class: 'col' }), side = h('aside', { class: 'col', 'aria-label': 'Claim facts' });
    var tag = CG.panels ? CG.panels.tag : function (el) { return el; };   // each part below can be dragged to where this person likes it (panels.js)
    var hitLines = [];
    flagged.forEach(function (f) { if (f.allowed_actions && f.allowed_actions.length) (f.affected_line_ids || []).forEach(function (l) { if (hitLines.indexOf(l) < 0) hitLines.push(l); }); });

    if (ctx.mode === 'work' && view.allowed_actions && view.allowed_actions.indexOf('verify_clear') >= 0) mainCol.appendChild(tag(greenActions(view, ctx, refresh), 'green', 'Clear or escalate this claim'));
    var confirmable = flagged.filter(canConfirm);
    if (hotkeyOff) hotkeyOff();
    if (flagged.length) {
      var decisions = tag(h('div', { class: 'stack decisions' }), 'decisions', 'Flagged findings and your decision');
      mainCol.appendChild(decisions);
      decisions.appendChild(h('div', { class: 'sectionhead' }, h('h2', { text: flagged.length + ' flagged finding' + (flagged.length > 1 ? 's' : '') }),
        ctx.mode === 'work' && confirmable.length > 1 ? confirmAll(confirmable, ctx, refresh) : null));
      // open findings first, then the most severe, so the next thing to do is always at the top
      flagged.slice().sort(function (a, b) {
        var oa = a.allowed_actions && a.allowed_actions.length ? 0 : 1, ob = b.allowed_actions && b.allowed_actions.length ? 0 : 1;
        return oa - ob || (a.severity === 'high' ? 0 : 1) - (b.severity === 'high' ? 0 : 1) || (a.rule_id < b.rule_id ? -1 : 1);
      }).forEach(function (f) { decisions.appendChild(findingCard(f, ctx, refresh, view.claim)); });
    }
    mainCol.appendChild(tag(linesCard(view.claim, hitLines), 'lines', 'Service lines'));
    if (fix) mainCol.appendChild(tag(fix, 'fix', 'Fix suggestions'));

    side.appendChild(tag(claimFacts(view.claim), 'facts', 'Claim facts'));
    var passed = view.findings.filter(function (f) { return flagged.indexOf(f) < 0; });
    if (passed.length) {
      var pass = h('details', { class: 'card' }, h('summary', { text: passed.length + ' other rules (passed or not applicable)' }),
        h('ul', { class: 'plain ruleref' }, passed.map(function (f) {
          return h('li', { title: f.explanation || '' }, CG.chip(f.status), ' ', h('span', { class: 'mono small', text: f.rule_id }), ' ', CG.ruleName(f.rule_id));
        })));
      if (CG.prefs.get('showPassed')) pass.open = true;
      side.appendChild(tag(pass, 'passed', 'Other rules'));
    }
    if (view.advisory && view.advisory.length) {
      var adv = view.advisory.filter(function (f) { return f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS'; });
      var advBox = h('details', { class: 'card' }, h('summary', { text: 'Advisory checks (information only): ' + adv.length + ' flagged' }),
        adv.length ? CG.table([{ label: 'Rule', get: function (f) { return h('span', { title: CG.ruleName(f.rule_id), text: f.rule_id }); } }, { label: 'Status', get: function (f) { return CG.chip(f.status); } },
          { label: 'Why', get: function (f) { return f.explanation; } }], adv) : h('p', { class: 'muted', text: 'Nothing flagged.' }));
      if (adv.length) advBox.open = true;
      side.appendChild(tag(advBox, 'advisory', 'Advisory checks'));
    }
    if (hist) mainCol.appendChild(tag(hist, 'history', 'Patient history'));      // under the lines: the table needs the width
    else if (CG.has('claims.decide')) mainCol.appendChild(tag(h('div', { class: 'card histcard' }, h('h2', { text: 'Patient history' }),
      h('p', { class: 'muted', text: 'The patient’s other claims open from your inbox: only the reviewer holding a claim may look at them. Open this claim from My inbox to see them.' })), 'history', 'Patient history'));
    var layoutEl = h('div', { class: 'worklayout' }, mainCol, side);
    root.appendChild(layoutEl);
    if (CG.panels) CG.panels.enable(layoutEl, { main: mainCol, side: side });
  };

  // The patient's other claims, asked for on purpose: reading them is logged and only the holder of the claim may. No patient identifier is shown.
  function historyCard(ctx) {
    var out = h('div', { class: 'stack' });
    var btn = h('button', { class: 'btn primary', type: 'button' }, CG.icon('list'), 'Show this patient’s other claims');
    var loaded = false;
    btn.addEventListener('click', function () {
      if (loaded) return;
      loaded = true; btn.disabled = true;
      CG.api('GET', '/work/claims/' + encodeURIComponent(ctx.id) + '/history').then(function (r) {
        CG.clear(out);
        btn.remove();
        if (!r.claims.length) { out.appendChild(h('p', { class: 'muted', text: 'This patient has no other claim on record: this is their first.' })); return; }
        var flaggedN = r.claims.filter(function (c) { return (c.flagged || []).length; }).length;
        out.appendChild(h('div', { class: 'histsum' },
          h('span', null, h('b', { text: String(r.count) }), r.count === 1 ? ' other claim' : ' other claims'),
          h('span', null, h('b', { class: flaggedN ? 'bad' : 'good', text: String(flaggedN) }), ' flagged by a rule'),
          h('span', null, 'latest ', h('b', { text: r.claims[0].submission_date || '-' }))));
        var open = CG.has('claims.view') ? function (c) { location.hash = '#/claim/' + encodeURIComponent(c.claim_id); } : null;
        out.appendChild(CG.table([
          { label: 'Claim', get: function (c) { return h('span', { class: 'mono cid', text: c.claim_id }); } },
          { label: 'Submitted', get: function (c) { return c.submission_date || '-'; } },
          { label: 'Provider', get: function (c) { return c.provider_id || '-'; } },
          { label: 'Total', num: true, get: function (c) { return c.total_amount === null || c.total_amount === undefined ? '-' : CG.money(c.total_amount, c.currency); } },
          { label: 'State', get: function (c) { return CG.chip(String(c.state || '-').replace(/_/g, ' '), c.state === 'decided' ? 'good' : 'neutral'); } },
          { label: 'Flagged', get: function (c) {
            var rules = (c.flagged || []).concat(c.advisory_flagged || []);
            return rules.length ? rules.map(function (x) { return h('span', { class: 'rid', title: CG.ruleName(x) || x, text: x }); }) : CG.chip('none', 'good');
          } }], r.claims, open ? { onRow: open } : {}));
        if (r.count > r.shown) out.appendChild(h('p', { class: 'muted small', text: 'Showing the newest ' + r.shown + ' of ' + r.count + '.' }));
        out.appendChild(h('p', { class: 'muted small' }, CG.icon('lock'), ' Looking at this was written to the security log.' + (open ? ' Click a row to open that claim.' : '')));
      }, function (e) { loaded = false; btn.disabled = false; CG.fail(e); });
    });
    var card = h('section', { class: 'card stack histcard', id: 'patient-history', tabindex: '-1' },
      h('div', null, h('h2', { text: 'Patient history' }), h('p', { class: 'muted small', text: 'This patient’s other claims: when, where, how much, and which rules flagged them. No identifiers are shown.' })),
      btn, out);
    card.load = function () { btn.click(); };
    return card;
  }

  // ---- inbox ------------------------------------------------------------------------------------------------------------
  function inboxRow(v) {
    var flagged = v.findings.filter(function (f) { return f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS'; }).length;
    var openList = v.findings.filter(function (f) { return f.allowed_actions && f.allowed_actions.length; });
    var high = openList.some(function (f) { return f.severity === 'high'; });
    return { id: v.claim_id, lane: v.lane, score: v.score, flagged: flagged, open: openList.length, left: v.leased_until, stage: v.signoff_stage, v: v,
      rules: openList.map(function (f) { return f.rule_id; }), sev: openList.length ? (high ? 'high' : 'medium') : null };
  }

  // The open findings of the inbox grouped by rule: same kind of finding side by side is faster to read than claim by claim.
  function ruleGroups(items) {
    var groups = {};
    items.forEach(function (v) {
      v.findings.forEach(function (f) {
        if (canConfirm(f)) (groups[f.rule_id] = groups[f.rule_id] || []).push({ id: v.claim_id, f: f });
      });
    });
    return groups;
  }

  function firstEvidence(f) {
    var e = f.evidence && f.evidence[0];
    return e ? e.path + ' = ' + CG.fmtValue(e.value) : '';
  }

  function ruleCard(rule, rows, light, reload) {
    var ctxFor = function (id) { return { decisionPath: function (r) { return '/work/claims/' + encodeURIComponent(id) + '/findings/' + encodeURIComponent(r) + '/decision'; } }; };
    var looked = false, bulk = null;
    var table = CG.table([
      { label: 'Claim', get: function (r) { return h('a', { href: '#/work/' + encodeURIComponent(r.id), class: 'mono cid', text: r.id }); } },
      { label: 'Severity', get: function (r) { return CG.chip(r.f.severity); } },
      { label: 'Status', get: function (r) { return CG.chip(r.f.status); } },
      { label: 'Lines', get: function (r) { return (r.f.affected_line_ids || []).join(', '); } },
      { label: 'What the engine saw', get: function (r) { return firstEvidence(r.f); } },
      { label: '', get: function (r) { return h('button', { class: 'btn small', type: 'button', text: 'Confirm', onclick: function () {
        confirmNow(ctxFor(r.id), r.f).then(function () { CG.toast('Confirmed ' + rule + ' on ' + r.id + '.', 'good'); reload(); }, CG.fail);
      } }); } }
    ], rows);
    var body = h('div', { class: 'stack' }, table);
    var head = h('div', { class: 'row between' },
      h('div', { class: 'row' }, h('span', { class: 'rid', text: rule }), h('b', { text: CG.ruleName(rule) }), h('span', { class: 'muted', text: rows.length + ' open finding' + (rows.length > 1 ? 's' : '') }),
        light ? CG.chip('Light review', 'green') : null));
    if (light) {
      bulk = twoStep('Confirm all ' + rows.length + ' after a light look', 'Click again to confirm all ' + rows.length, function (b) {
        if (!looked) { CG.toast('Open the list and look at the findings first.', 'warn'); return; }
        b.disabled = true;
        var ids = rows.map(function (r) { return r.id; }), chunks = [];
        while (ids.length) chunks.push(ids.splice(0, 25));
        var done = { confirmed: 0, full: 0, refused: 0 };
        chunks.reduce(function (p, c) {
          return p.then(function () { return CG.api('POST', '/work/bulk-confirm', { rule_id: rule, claim_ids: c }).then(function (d) {
            done.confirmed += d.confirmed; done.full += d.left_for_full_review; done.refused += d.refused;
          }); });
        }, Promise.resolve()).then(function () {
          CG.toast('Confirmed ' + done.confirmed + '. ' + done.full + ' left for you to review fully (a random check). ' + done.refused + ' refused.', done.refused ? 'warn' : 'good'); reload();
        }, function (e) { b.disabled = false; CG.fail(e); reload(); });
      });
      head.appendChild(bulk);
    }
    var details = h('details', null, h('summary', { text: light ? 'Look at these findings (needed before bulk confirm)' : 'Show the findings' }), body);
    details.addEventListener('toggle', function () { if (details.open) looked = true; });
    return h('div', { class: 'card rulecard' }, head,
      light ? h('p', { class: 'muted small', text: 'Reviewers have upheld almost every finding of this rule. A random few are still sent to you for a full review, so the check never stops.' }) : null,
      details);
  }

  CG.views = CG.views || {};
  CG.views.inbox = function (root) {
    var list = h('div'), status = h('div', { class: 'tiles' });
    var mode = CG.state.inboxMode || CG.prefs.get('inboxMode') || 'claim', light = [], lastSig = '';
    // The page refreshes itself every 20 seconds and on server pushes. In the by-rule view a redraw would close the list the person has open and
    // forget that they looked at it, so it redraws only when something that matters changed.
    function signature(d) {
      return JSON.stringify([d.light_review || [], d.claims.map(function (v) {
        return [v.claim_id, v.version, v.signoff_stage, v.findings.map(function (f) { return [f.rule_id, f.review_state, !!(f.allowed_actions && f.allowed_actions.length)]; })];
      })]);
    }
    function drawByRule(items) {
      var groups = ruleGroups(items), rules = Object.keys(groups).sort();
      if (!rules.length) { list.appendChild(CG.empty('check', 'No open findings to confirm', 'Switch to "By claim" to verify clean claims.')); return; }
      rules.forEach(function (rule) { list.appendChild(ruleCard(rule, groups[rule], light.indexOf(rule) >= 0, load)); });
    }
    function draw(items) {
      CG.state.inbox = items;
      CG.clear(list);
      if (mode === 'rule' && items.length) { drawByRule(items); return; }
      if (!items.length) {
        list.appendChild(CG.empty('inbox', 'Your inbox is empty', 'The dispatcher keeps it topped up while you are on shift. Press "Take next claim" to ask for one now.'));
        return;
      }
      var hiddenCols = CG.prefs.get('hiddenCols') || [];      // personal choice of inbox columns (Customize, Lists); the claim number always stays
      list.appendChild(h('div', { class: 'card' }, CG.table([
        { label: 'Claim', get: function (r) { return h('span', { class: 'mono cid', text: r.id }); } },
        { key: 'lane', label: 'Lane', get: function (r) { return CG.chip(r.lane === 'green' ? 'clean' : 'Lane ' + r.lane, r.lane); } },
        { key: 'issues', label: 'Issues', get: function (r) {
          if (!r.rules.length) return h('span', { class: 'muted', text: r.flagged ? 'with someone else' : 'verify clean claim' });
          return h('span', { title: r.rules.map(function (x) { return x + ' ' + CG.ruleName(x); }).join('\n') }, CG.chip(r.sev), ' ', r.rules.length === 1 ? [h('span', { class: 'rid', text: r.rules[0] }), ' ' + CG.ruleName(r.rules[0])] : r.rules.map(function (x) { return h('span', { class: 'rid', text: x }); }));
        } },
        { key: 'todo', label: 'To decide', num: true, get: function (r) { return r.open || (r.flagged ? 0 : 'verify'); } },
        { key: 'priority', label: 'Priority', num: true, get: function (r) { return h('span', { title: 'Priority score: a higher number is looked at sooner. It comes from severity and how long the claim has waited.', text: String(r.score) }); } },
        { key: 'stage', label: 'Stage', get: function (r) { return r.stage ? CG.chip(r.stage, 'B') : null; } },
        { key: 'lease', label: 'Time left', get: function (r) { return r.left ? h('span', { title: 'Your hold on this claim. When it runs out the claim goes back to the pool for someone else.', text: CG.fmtDuration(r.left - Date.now() / 1000) }) : null; } }
      ].filter(function (c) { return !c.key || hiddenCols.indexOf(c.key) < 0; }), items.map(inboxRow), { onRow: function (r) { location.hash = '#/work/' + encodeURIComponent(r.id); } })));
    }
    function tile(n, label, hl) { return h('div', { class: 'tile' + (hl ? ' hl-' + hl : '') }, h('div', { class: 'n', text: String(n) }), h('div', { class: 'l', text: label })); }
    function load() {
      return CG.api('GET', '/work/inbox').then(function (d) {
        light = d.light_review || [];
        var rows = d.claims.map(inboxRow), open = 0, green = 0, staged = 0;
        rows.forEach(function (r) { open += r.open; if (!r.flagged) green++; if (r.stage) staged++; });
        CG.clear(status);
        CG.append(status, [tile(d.claims.length, d.claims.length === 1 ? 'claim waiting for you' : 'claims waiting for you'), tile(open, 'findings to decide', open ? 'high' : 'ok'),
          tile(green, 'clean claims to verify', green ? 'ok' : null), staged ? tile(staged, 'waiting on a second senior', 'med') : null]);
        var sig = signature(d);
        if (mode === 'rule' && sig === lastSig && list.firstChild) { CG.state.inbox = d.claims; return; }
        lastSig = sig;
        draw(d.claims);
      }, CG.fail);
    }
    var take = h('button', { class: 'btn primary', type: 'button', onclick: function () {
      CG.api('POST', '/work/next').then(function (d) {
        if (!d.claim) { CG.toast('Nothing is waiting for you right now.', 'info'); return; }
        location.hash = '#/work/' + encodeURIComponent(d.claim.claim_id);
      }, CG.fail);
    } }, CG.icon('bolt'), 'Take next claim');
    var byClaim = h('button', { class: 'btn small', type: 'button', text: 'By claim' }), byRule = h('button', { class: 'btn small', type: 'button', text: 'By rule' });
    function paint() { byClaim.setAttribute('aria-pressed', String(mode === 'claim')); byRule.setAttribute('aria-pressed', String(mode === 'rule')); }
    function pick(m) { mode = m; CG.state.inboxMode = m; lastSig = signature({ claims: CG.state.inbox || [], light_review: light }); paint(); draw(CG.state.inbox || []); }
    byClaim.addEventListener('click', function () { pick('claim'); });
    byRule.addEventListener('click', function () { pick('rule'); });
    paint();
    root.appendChild(CG.pageHead({ title: 'My inbox', sub: 'Claims routed to you. Open one, or take the next in line.',
      actions: [h('div', { class: 'seg', role: 'group', 'aria-label': 'Group the inbox' }, byClaim, byRule), take] }));
    root.appendChild(status); list.appendChild(CG.loading('Reading your inbox...'));
    root.appendChild(h('div', { class: 'sectionhead' })); root.appendChild(list);
    load();
    CG.every(20000, load);
    CG.live('inbox', load);
    CG.every(60000, function () { CG.api('POST', '/work/heartbeat').catch(function () {}); });
  };

  CG.views.work = function (root, id) {
    var ctx = {
      id: id, mode: 'work',
      decisionPath: function (rule) { return '/work/claims/' + encodeURIComponent(id) + '/findings/' + encodeURIComponent(rule) + '/decision'; },
      fixPath: function (what) { return '/work/claims/' + encodeURIComponent(id) + '/' + what; },
      reload: function () { return CG.api('GET', '/work/inbox').then(function (d) { return d.claims.filter(function (c) { return c.claim_id === id; })[0] || null; }); }
    };
    root.appendChild(CG.loading('Loading claim...'));
    ctx.reload().then(function (v) {
      CG.clear(root);
      if (!v) { root.appendChild(CG.empty('inbox', 'This claim is not in your inbox', 'It may be finished, or its lease ran out.', h('a', { class: 'btn', href: '#/inbox', text: 'Back to my inbox' }))); return; }
      CG.renderClaim(root, v, ctx);
      CG.every(60000, function () { CG.api('POST', '/work/heartbeat').catch(function () {}); });
    }, CG.fail);
  };

  // ---- browse -----------------------------------------------------------------------------------------------------------
  CG.views.claims = function (root) {
    var status = h('select', { 'aria-label': 'Status' }, ['', 'FAIL', 'UNABLE_TO_ASSESS', 'PASS'].map(function (s) { return h('option', { value: s, text: s ? s.replace(/_/g, ' ') : 'Any status' }); }));
    var rule = h('select', { 'aria-label': 'Rule' }, h('option', { value: '', text: 'Any rule' }),
      RULE_CHOICES.map(function (r) { return h('option', { value: r, text: r + ' · ' + CG.ruleName(r) }); }));
    var handed = CG.state.claimsFilter; CG.state.claimsFilter = null;      // a filter chosen on the Home page
    if (handed && handed.status) status.value = handed.status;
    var find = h('input', { type: 'search', 'aria-label': 'Find a claim by number', placeholder: 'Claim number, e.g. CG-1CB0', class: 'findbox', autocomplete: 'off' });
    var out = h('div', { class: 'card' }), pager = h('div', { class: 'row savebar' }), offset = 0, limit = 25;
    function load() {
      var q = '?limit=' + limit + '&offset=' + offset + (status.value ? '&status=' + encodeURIComponent(status.value) : '') + (rule.value ? '&rule_id=' + encodeURIComponent(rule.value) : '');
      CG.clear(out); out.appendChild(CG.loading());
      CG.api('GET', '/claims' + q).then(function (d) {
        CG.clear(out); CG.clear(pager);
        var rows = d.claims;
        if (!rows.length) { out.appendChild(h('div', { class: 'empty' }, h('h2', { text: 'No claims match' }), h('p', { class: 'muted', text: 'Try another status or rule.' }))); }
        else {
          var cols = [
            { label: 'Claim', get: function (r) { return h('span', { class: 'mono cid', text: r.claim_id }); } },
            { label: 'Provider', get: function (r) { return r.provider_id || null; } },
            { label: 'Submitted', get: function (r) { return r.submission_date || null; } },
            { label: 'Amount', get: function (r) { return typeof r.total_amount === 'number' ? CG.money(r.total_amount, r.currency) : null; } },
            { label: 'Results', get: function (r) { return Object.keys(r.counts || {}).sort().map(function (k) { return r.counts[k] + ' ' + k.replace(/_/g, ' ').toLowerCase(); }).join(', '); } },
            { label: 'Highest severity', get: function (r) { return r.max_severity ? CG.chip(r.max_severity) : null; } },
            { label: 'Flagged', get: function (r) { return r.flagged ? CG.chip('flagged', 'high') : 'no'; } }
          ];
          out.appendChild(CG.table(cols, rows, { onRow: function (r) { location.hash = '#/claim/' + encodeURIComponent(r.claim_id); } }));
        }
        pager.appendChild(h('button', { class: 'btn small', type: 'button', text: 'Previous', disabled: offset === 0, onclick: function () { offset = Math.max(0, offset - limit); load(); } }));
        pager.appendChild(h('button', { class: 'btn small', type: 'button', text: 'Next', disabled: rows.length < limit, onclick: function () { offset += limit; load(); } }));
        pager.appendChild(h('span', { class: 'muted small', text: 'Showing ' + (offset + 1) + ' to ' + (offset + rows.length) }));
      }, CG.fail);
    }
    // typing narrows the rows on this page at once; Enter on a full claim number opens that claim
    find.addEventListener('input', function () {
      var q = find.value.trim().toUpperCase();
      Array.prototype.forEach.call(out.querySelectorAll('tbody tr'), function (tr) { tr.hidden = !!q && tr.textContent.toUpperCase().indexOf(q) < 0; });
    });
    find.addEventListener('keydown', function (e) {
      var q = find.value.trim().toUpperCase();
      if (e.key === 'Enter' && /^CG-[0-9A-Z]{6,}$/.test(q)) location.hash = '#/claim/' + encodeURIComponent(q);
    });
    status.addEventListener('change', function () { offset = 0; load(); });
    rule.addEventListener('change', function () { offset = 0; load(); });
    root.appendChild(CG.pageHead({ title: 'All claims', sub: CG.has('claims.decide') ? 'Every claim and its results. Your own work is in My inbox.' : 'Every claim and its results, read-only.' }));
    root.appendChild(h('div', { class: 'row', role: 'search' }, h('div', null, find), h('div', null, status), h('div', null, rule)));
    root.appendChild(h('div', { class: 'sectionhead' })); root.appendChild(out); root.appendChild(pager);
    load();
  };

  CG.views.claim = function (root, id) {
    var ctx = {
      id: id, mode: 'browse',
      decisionPath: function (rule) { return '/claims/' + encodeURIComponent(id) + '/findings/' + encodeURIComponent(rule) + '/decision'; },
      reload: function () { return CG.api('GET', '/claims/' + encodeURIComponent(id)); }
    };
    root.appendChild(CG.loading('Loading claim...'));
    ctx.reload().then(function (v) { CG.clear(root); CG.renderClaim(root, v, ctx); }, function (e) { CG.clear(root); root.appendChild(h('div', { class: 'notice bad', text: e.message })); });
  };
})();
