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
    function sync() { send.disabled = busy || !chosen || !reason.value.trim(); }
    reason.addEventListener('input', sync);
    send.addEventListener('click', function () {
      busy = true; sync();
      CG.api('POST', ctx.decisionPath(finding.rule_id), { action: chosen, reason: reason.value.trim() }).then(function () {
        CG.toast('Decision recorded for ' + finding.rule_id + '.', 'good'); refresh();
      }, function (e) { busy = false; sync(); CG.fail(e); });
    });
    return h('div', { class: 'stack' }, h('h3', { text: 'Your decision' }), choices, h('div', { class: 'field' }, h('label', { text: 'Reason (kept in the audit log)' }), reason), send);
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
    if (f.ai_explanation) {
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

  function greenActions(view, ctx, refresh) {
    function send(action) {
      CG.api('POST', '/work/claims/' + encodeURIComponent(ctx.id) + '/verify', { action: action }).then(function () {
        CG.toast(action === 'verify_clear' ? 'Marked clear.' : 'Sent to a senior reviewer.', 'good'); refresh();
      }, CG.fail);
    }
    return h('div', { class: 'card flag-pass stack' },
      h('h2', { text: 'No rule flagged this claim' }),
      h('p', { text: 'All findings passed. A person still looks at every claim: confirm it is clear, or send it up if something looks wrong to you.' }),
      h('div', { class: 'row' },
        h('button', { class: 'btn primary', type: 'button', text: 'Verify: this claim is clear', onclick: function () { send('verify_clear'); } }),
        h('button', { class: 'btn', type: 'button', text: 'Escalate to a senior', onclick: function () { send('escalate'); } })));
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
        if (!v) { CG.toast('This claim is finished and has left your inbox.', 'good'); location.hash = back; return; }
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

    root.appendChild(claimSummary(view.claim));
    if (ctx.mode === 'work' && view.allowed_actions && view.allowed_actions.indexOf('verify_clear') >= 0) root.appendChild(greenActions(view, ctx, refresh));

    root.appendChild(h('h2', { text: flagged.length ? flagged.length + ' flagged finding' + (flagged.length > 1 ? 's' : '') : 'Findings', style: null }));
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

  CG.views = CG.views || {};
  CG.views.inbox = function (root) {
    var list = h('div'), status = h('p', { class: 'muted' });
    function draw(items) {
      CG.state.inbox = items;
      CG.clear(list);
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
      return CG.api('GET', '/work/inbox').then(function (d) { draw(d.claims); status.textContent = d.claims.length + ' claim' + (d.claims.length === 1 ? '' : 's') + ' waiting for you.'; }, CG.fail);
    }
    var take = h('button', { class: 'btn primary', type: 'button', text: 'Take next claim', onclick: function () {
      CG.api('POST', '/work/next').then(function (d) {
        if (!d.claim) { CG.toast('Nothing is waiting for you right now.', 'info'); return; }
        location.hash = '#/work/' + encodeURIComponent(d.claim.claim_id);
      }, CG.fail);
    } });
    root.appendChild(h('div', { class: 'row between' }, h('h1', { text: 'My inbox' }), take));
    root.appendChild(status); root.appendChild(list);
    load();
    CG.every(20000, load);
    CG.every(60000, function () { CG.api('POST', '/work/heartbeat').catch(function () {}); });
  };

  CG.views.work = function (root, id) {
    var ctx = {
      id: id, mode: 'work',
      decisionPath: function (rule) { return '/work/claims/' + encodeURIComponent(id) + '/findings/' + encodeURIComponent(rule) + '/decision'; },
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
          var cols = Object.keys(rows[0]).map(function (k) { return { label: k.replace(/_/g, ' '), get: function (r) { return CG.fmtValue(r[k]); } }; });
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
