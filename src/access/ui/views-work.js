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
      h('span', { class: 'muted small', text: 'Use this only when every finding below is right. It records one confirm per finding. Keyboard: press c twice.' }));
  }

  // ---- one claim ------------------------------------------------------------------------------------------------------
  function claimSummary(c) {
    var cov = c.coverage || {};
    var lines = c.lines || [];
    return h('div', { class: 'card' },
      h('h2', { text: 'Claim details' }),
      h('div', { class: 'row', style: null },
        h('div', { class: 'grow' }, CG.kv([['Provider', c.provider_id], ['Payer', c.payer_id], ['Policy', c.policy_id], ['Diagnosis', c.diagnosis_code], ['Invoice', c.invoice_number]])),
        h('div', { class: 'grow' }, CG.kv([['Submitted', c.submission_date], ['Total', CG.money(c.total_amount, c.currency)],
          ['Patient', c.patient_id], ['Member', c.member_id], ['Coverage', cov.status ? cov.status + ' ' + (cov.start_date || '?') + ' to ' + (cov.end_date || '?') : '']]))),
      h('h3', { text: 'Lines', style: null }),
      CG.table([
        { label: 'Line', get: function (l) { return l.line_id; } }, { label: 'Service', get: function (l) { return l.service_code; } },
        { label: 'Date', get: function (l) { return l.service_date; } }, { label: 'Modifier', get: function (l) { return l.modifier; } },
        { label: 'Qty', num: true, get: function (l) { return l.quantity; } }, { label: 'Unit price', num: true, get: function (l) { return l.unit_price; } },
        { label: 'Net', num: true, get: function (l) { return l.net_amount; } }, { label: 'Authorization', get: function (l) { return l.authorization_id; } }
      ], lines),
      c.notes ? h('div', { class: 'notice', style: null }, h('b', { text: 'Notes: ' }), c.notes) : null);
  }

  function evidenceTable(list) {
    if (!list || !list.length) return null;
    return h('div', { class: 'evidence' }, CG.table([
      { label: 'Where', get: function (e) { return e.path; } }, { label: 'Value', get: function (e) { return CG.fmtValue(e.value); } }], list));
  }

  function decisionForm(finding, ctx, refresh) {
    var chosen = null, busy = false;
    var reason = h('textarea', { maxlength: '1000', placeholder: 'Why? Write what you checked and what you found.', 'aria-label': 'Reason' });
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
    function sync() { send.disabled = busy || !chosen || (chosen !== 'confirm_issue' && !reason.value.trim()); }
    reason.addEventListener('input', sync);
    send.addEventListener('click', function () {
      busy = true; sync();
      CG.api('POST', ctx.decisionPath(finding.rule_id), { action: chosen, reason: reason.value.trim() }).then(function () {
        CG.toast('Decision recorded for ' + finding.rule_id + '.', 'good'); refresh();
      }, function (e) { busy = false; sync(); CG.fail(e); });
    });
    var quick = canConfirm(finding) ? h('div', { class: 'row' },
      h('button', { class: 'btn primary', type: 'button', text: 'Confirm the issue', onclick: function () {
        if (busy) return;
        busy = true; sync();
        confirmNow(ctx, finding).then(function () { CG.toast('Confirmed ' + finding.rule_id + '.', 'good'); refresh(); }, function (e) { busy = false; sync(); CG.fail(e); });
      } }),
      h('span', { class: 'muted small', text: 'One click when the finding is right. No reason needed.' })) : null;
    return h('div', { class: 'stack' }, h('h3', { text: 'Your decision' }), quick,
      h('p', { class: 'muted small', text: quick ? 'Or choose another action (a reason is needed to dismiss).' : 'Choose an action.' }), choices,
      h('div', { class: 'field' }, h('label', { text: 'Reason (kept in the audit log; optional when you confirm)' }), reason), send);
  }

  function findingCard(f, ctx, refresh) {
    var flagged = f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS';
    var kind = !flagged ? 'flag-pass' : f.severity === 'high' ? 'flag-high' : 'flag-medium';
    var card = h('div', { class: 'card ' + kind },
      h('div', { class: 'row between' },
        h('div', { class: 'row' }, h('b', { class: 'mono', text: f.rule_id }), CG.chip(f.status), flagged ? CG.chip(f.severity) : null, f.review_state ? CG.chip(f.review_state) : null),
        f.affected_line_ids && f.affected_line_ids.length ? h('span', { class: 'muted small', text: 'Lines: ' + f.affected_line_ids.join(', ') }) : null),
      f.explanation ? h('p', { text: f.explanation, style: null }) : null);
    if (!flagged) return card;
    if (f.ai_explanation && f.ai_explanation.text !== f.explanation) {      // the built-in template restates the line above: show the box only when it adds something
      card.appendChild(h('div', { class: 'ai-box' },
        h('div', { class: 'row' }, CG.chip('AI draft', 'ai'), h('span', { class: 'muted small', text: 'Source: ' + f.ai_explanation.source + '. The rule engine decided the status; this text only helps you read it. Check it against the evidence.' })),
        h('p', { text: f.ai_explanation.text })));
    }
    if (f.corrective_action) card.appendChild(h('p', null, h('b', { text: 'What the rulebook says to do: ' }), f.corrective_action));
    var ev = evidenceTable(f.evidence);
    if (ev) card.appendChild(h('details', null, h('summary', { text: 'Evidence (' + f.evidence.length + ')' }), ev));
    if (f.allowed_actions && f.allowed_actions.length) card.appendChild(decisionForm(f, ctx, refresh));
    else if (f.review_state === 'resolved') card.appendChild(h('p', { class: 'muted small', text: 'Settled.' }));
    else card.appendChild(h('p', { class: 'muted small', text: 'You cannot decide this finding with your clearance (hidden actions are not offered).' }));
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
    return h('div', { class: 'card flag-pass stack' },
      h('h2', { text: 'No rule flagged this claim' }),
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
    var head = h('div', { class: 'row between' },
      h('div', null, h('a', { href: back, text: ctx.mode === 'work' ? 'Back to my inbox' : 'Back to all claims' }), h('h1', { class: 'mono', text: ctx.id })),
      h('div', { class: 'row' }, view.lane ? CG.chip('Lane ' + view.lane, view.lane === 'green' ? 'green' : view.lane) : null, view.score !== undefined ? h('span', { class: 'muted', text: 'score ' + view.score }) : null,
        view.version ? h('span', { class: 'muted', text: 'version ' + view.version }) : null));
    root.appendChild(head);

    if (view.leased_until) {
      var lease = h('span', { class: 'mono' });
      var tick = function () { var left = view.leased_until - Date.now() / 1000; lease.textContent = left > 0 ? CG.fmtDuration(left) + ' left on your lease' : 'Your lease has run out'; };
      tick(); CG.every(1000, tick);
      root.appendChild(h('div', { class: 'notice info row between' }, h('span', null, lease), h('span', { class: 'muted small', text: 'Staying on this page keeps the lease alive. If it lapses the claim goes back to the pool.' })));
    }
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
    root.appendChild(claimSummary(view.claim));
    if (fix) root.appendChild(fix);
    if (ctx.mode === 'work' && view.allowed_actions && view.allowed_actions.indexOf('verify_clear') >= 0) root.appendChild(greenActions(view, ctx, refresh));

    root.appendChild(h('h2', { text: flagged.length ? flagged.length + ' flagged finding' + (flagged.length > 1 ? 's' : '') : 'Findings', style: null }));
    var confirmable = flagged.filter(canConfirm);
    if (hotkeyOff) hotkeyOff();
    if (ctx.mode === 'work' && confirmable.length > 1) root.appendChild(confirmAll(confirmable, ctx, refresh));
    flagged.forEach(function (f) { root.appendChild(findingCard(f, ctx, refresh)); });
    var passed = view.findings.filter(function (f) { return flagged.indexOf(f) < 0; });
    if (passed.length) {
      root.appendChild(h('details', { class: 'card' }, h('summary', { text: passed.length + ' other rules (passed or not applicable)' }),
        CG.table([{ label: 'Rule', get: function (f) { return f.rule_id; } }, { label: 'Status', get: function (f) { return CG.chip(f.status); } },
          { label: 'Why', get: function (f) { return f.explanation; } }], passed)));
    }
    if (view.advisory && view.advisory.length) {
      var adv = view.advisory.filter(function (f) { return f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS'; });
      root.appendChild(h('details', { class: 'card' }, h('summary', { text: 'Advisory checks (extra rules, for information only): ' + adv.length + ' flagged' }),
        adv.length ? CG.table([{ label: 'Rule', get: function (f) { return f.rule_id; } }, { label: 'Status', get: function (f) { return CG.chip(f.status); } },
          { label: 'Why', get: function (f) { return f.explanation; } }], adv) : h('p', { class: 'muted', text: 'Nothing flagged.' })));
    }
  };

  // ---- inbox ------------------------------------------------------------------------------------------------------------
  function inboxRow(v) {
    var flagged = v.findings.filter(function (f) { return f.status === 'FAIL' || f.status === 'UNABLE_TO_ASSESS'; }).length;
    var open = v.findings.filter(function (f) { return f.allowed_actions && f.allowed_actions.length; }).length;
    return { id: v.claim_id, lane: v.lane, score: v.score, flagged: flagged, open: open, left: v.leased_until, stage: v.signoff_stage, v: v };
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
      { label: 'Claim', get: function (r) { return h('a', { href: '#/work/' + encodeURIComponent(r.id), class: 'mono', text: r.id }); } },
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
      h('div', { class: 'row' }, h('b', { class: 'mono', text: rule }), h('span', { class: 'muted', text: rows.length + ' open finding' + (rows.length > 1 ? 's' : '') }),
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
    return h('div', { class: 'card' }, head,
      light ? h('p', { class: 'muted small', text: 'Reviewers have upheld almost every finding of this rule. A random few are still sent to you for a full review, so the check never stops.' }) : null,
      details);
  }

  CG.views = CG.views || {};
  CG.views.inbox = function (root) {
    var list = h('div'), status = h('p', { class: 'muted' });
    var mode = CG.state.inboxMode || 'claim', light = [], lastSig = '';
    // The page refreshes itself every 20 seconds and on server pushes. In the by-rule view a redraw would close the list the person has open and
    // forget that they looked at it, so it redraws only when something that matters changed.
    function signature(d) {
      return JSON.stringify([d.light_review || [], d.claims.map(function (v) {
        return [v.claim_id, v.version, v.signoff_stage, v.findings.map(function (f) { return [f.rule_id, f.review_state, !!(f.allowed_actions && f.allowed_actions.length)]; })];
      })]);
    }
    function drawByRule(items) {
      var groups = ruleGroups(items), rules = Object.keys(groups).sort();
      if (!rules.length) { list.appendChild(h('div', { class: 'card' }, h('p', { text: 'No open findings to confirm in your inbox.' }))); return; }
      rules.forEach(function (rule) { list.appendChild(ruleCard(rule, groups[rule], light.indexOf(rule) >= 0, load)); });
    }
    function draw(items) {
      CG.state.inbox = items;
      CG.clear(list);
      if (mode === 'rule' && items.length) { drawByRule(items); return; }
      if (!items.length) {
        list.appendChild(h('div', { class: 'card' }, h('p', { text: 'Your inbox is empty.' }), h('p', { class: 'muted', text: 'The dispatcher keeps it topped up while you are on shift. Press "Take next claim" to ask for one now.' })));
        return;
      }
      list.appendChild(CG.table([
        { label: 'Claim', get: function (r) { return h('span', { class: 'mono', text: r.id }); } },
        { label: 'Lane', get: function (r) { return CG.chip(r.lane === 'green' ? 'green' : 'Lane ' + r.lane, r.lane); } },
        { label: 'Score', num: true, get: function (r) { return r.score; } },
        { label: 'Flagged', num: true, get: function (r) { return r.flagged; } },
        { label: 'To decide', num: true, get: function (r) { return r.open || (r.flagged ? 0 : 'verify'); } },
        { label: 'Stage', get: function (r) { return r.stage ? CG.chip(r.stage, 'B') : null; } },
        { label: 'Lease', get: function (r) { return r.left ? CG.fmtDuration(r.left - Date.now() / 1000) : null; } }
      ], items.map(inboxRow), { onRow: function (r) { location.hash = '#/work/' + encodeURIComponent(r.id); } }));
    }
    function load() {
      return CG.api('GET', '/work/inbox').then(function (d) {
        light = d.light_review || [];
        status.textContent = d.claims.length + ' claim' + (d.claims.length === 1 ? '' : 's') + ' waiting for you.';
        var sig = signature(d);
        if (mode === 'rule' && sig === lastSig && list.firstChild) { CG.state.inbox = d.claims; return; }
        lastSig = sig;
        draw(d.claims);
      }, CG.fail);
    }
    var take = h('button', { class: 'btn primary', type: 'button', text: 'Take next claim', onclick: function () {
      CG.api('POST', '/work/next').then(function (d) {
        if (!d.claim) { CG.toast('Nothing is waiting for you right now.', 'info'); return; }
        location.hash = '#/work/' + encodeURIComponent(d.claim.claim_id);
      }, CG.fail);
    } });
    var byClaim = h('button', { class: 'btn small', type: 'button', text: 'By claim' }), byRule = h('button', { class: 'btn small', type: 'button', text: 'By rule' });
    function paint() { byClaim.className = 'btn small' + (mode === 'claim' ? ' primary' : ''); byRule.className = 'btn small' + (mode === 'rule' ? ' primary' : ''); }
    function pick(m) { mode = m; CG.state.inboxMode = m; lastSig = signature({ claims: CG.state.inbox || [], light_review: light }); paint(); draw(CG.state.inbox || []); }
    byClaim.addEventListener('click', function () { pick('claim'); });
    byRule.addEventListener('click', function () { pick('rule'); });
    paint();
    root.appendChild(h('div', { class: 'row between' }, h('h1', { text: 'My inbox' }), h('div', { class: 'row' }, byClaim, byRule, take)));
    root.appendChild(status); root.appendChild(list);
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
    root.appendChild(h('p', { class: 'muted', text: 'Loading claim...' }));
    ctx.reload().then(function (v) {
      CG.clear(root);
      if (!v) { root.appendChild(h('div', { class: 'card' }, h('p', { text: 'This claim is not in your inbox. It may be finished, or its lease ran out.' }), h('a', { href: '#/inbox', text: 'Back to my inbox' }))); return; }
      CG.renderClaim(root, v, ctx);
      CG.every(60000, function () { CG.api('POST', '/work/heartbeat').catch(function () {}); });
    }, CG.fail);
  };

  // ---- browse -----------------------------------------------------------------------------------------------------------
  CG.views.claims = function (root) {
    var status = h('select', { 'aria-label': 'Status' }, ['', 'FAIL', 'UNABLE_TO_ASSESS', 'PASS'].map(function (s) { return h('option', { value: s, text: s ? s.replace(/_/g, ' ') : 'Any status' }); }));
    var rule = h('input', { type: 'text', placeholder: 'Rule, e.g. R003', maxlength: '10', 'aria-label': 'Rule' });
    var out = h('div'), pager = h('div', { class: 'row' }), offset = 0, limit = 25;
    function load() {
      var q = '?limit=' + limit + '&offset=' + offset + (status.value ? '&status=' + encodeURIComponent(status.value) : '') + (rule.value.trim() ? '&rule_id=' + encodeURIComponent(rule.value.trim().toUpperCase()) : '');
      CG.api('GET', '/claims' + q).then(function (d) {
        CG.clear(out); CG.clear(pager);
        var rows = d.claims;
        if (!rows.length) { out.appendChild(h('p', { class: 'muted', text: 'No claims match.' })); }
        else {
          var cols = [
            { label: 'Claim', get: function (r) { return h('span', { class: 'mono', text: r.claim_id }); } },
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
    status.addEventListener('change', function () { offset = 0; load(); });
    rule.addEventListener('keydown', function (e) { if (e.key === 'Enter') { offset = 0; load(); } });
    root.appendChild(h('h1', { text: 'All claims' }));
    root.appendChild(h('div', { class: 'row' }, h('div', null, status), h('div', null, rule), h('button', { class: 'btn', type: 'button', text: 'Filter', onclick: function () { offset = 0; load(); } })));
    root.appendChild(out); root.appendChild(pager);
    load();
  };

  CG.views.claim = function (root, id) {
    var ctx = {
      id: id, mode: 'browse',
      decisionPath: function (rule) { return '/claims/' + encodeURIComponent(id) + '/findings/' + encodeURIComponent(rule) + '/decision'; },
      reload: function () { return CG.api('GET', '/claims/' + encodeURIComponent(id)); }
    };
    root.appendChild(h('p', { class: 'muted', text: 'Loading claim...' }));
    ctx.reload().then(function (v) { CG.clear(root); CG.renderClaim(root, v, ctx); }, function (e) { CG.clear(root); root.appendChild(h('div', { class: 'notice bad', text: e.message })); });
  };
})();
