/* ClaimGuard console: the copilot panel (what is wrong, what to ask or correct) and the correct-and-recheck panel (what-if, then recheck as a new version).
 * Same rule as the rest of this folder: nothing from the server is ever put on the page as markup. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;

  var KIND = { correct_value: 'a value to correct', verify: 'check against the source', request_information: 'ask the provider' };
  var OUTCOME = { resolved: ['cleared', 'good'], still_flagged: ['still flagged', 'medium'], newly_flagged: ['NEW problem', 'high'], unchanged: ['unchanged', 'muted'] };

  function detail(e) { return (e && e.data && e.data.detail) ? e.data.detail : (e && e.message) || 'Something went wrong.'; }

  // ---- the copilot: summary and one suggestion per finding ----------------------------------------------------------------
  CG.copilotPanel = function (copilot, onTry) {
    if (!copilot || !copilot.summary) return null;
    var s = copilot.summary;
    var card = h('div', { class: 'card copilot' },
      h('div', { class: 'row between' }, h('h2', { text: 'Reviewer copilot' }), CG.chip('built from the findings, no model', 'ai')),
      h('p', null, h('b', { text: s.headline })));
    if (s.cleared) return card;
    var by = {};
    (copilot.suggestions || []).forEach(function (x) { by[x.rule_id] = x; });
    card.appendChild(h('ol', { class: 'plain steps' }, (s.items || []).map(function (it) {
      var sug = by[it.rule_id] || {};
      var edits = sug.proposed_edits || [];
      return h('li', null,
        h('div', { class: 'row' }, h('b', { class: 'mono', text: it.rule_id }), CG.chip(it.severity), CG.chip(KIND[it.kind] || it.kind, 'info')),
        h('p', { text: String(it.sentence).replace(/^R\d{3}\s+(failed|could not be assessed)\s+\((high|medium) severity\):\s*/, '') }),
        (sug.options && sug.options.length) ? h('p', { class: 'muted small', text: 'Closest catalogue codes (not applied): ' + sug.options.join(', ') }) : null,
        edits.length && onTry ? h('div', { class: 'row' },
          h('button', { class: 'btn small', type: 'button', text: 'Try this in a what-if', onclick: function () { onTry(edits); } }),
          h('span', { class: 'muted small', text: edits.map(function (e) { return e.path + ': ' + CG.fmtValue(e.from) + ' to ' + CG.fmtValue(e.to) + ' (' + e.basis + ')'; }).join('; ') })) : null);
    })));
    if (s.next_steps && s.next_steps.length) {
      card.appendChild(h('h3', { text: 'In this order' }));
      card.appendChild(h('ul', { class: 'plain' }, s.next_steps.map(function (t) { return h('li', { text: t }); })));
    }
    card.appendChild(h('p', { class: 'muted small', text: 'A proposed value is only ever worked out from other values on this claim (or the policy currency). Nothing missing is filled in, and no identifier can be edited here.' }));
    return card;
  };

  // ---- correct and recheck -----------------------------------------------------------------------------------------------
  var HEADER = [['total_amount', 'Total'], ['currency', 'Currency'], ['submission_date', 'Submitted'], ['invoice_number', 'Invoice'], ['diagnosis_code', 'Diagnosis'], ['provider_id', 'Provider'], ['policy_id', 'Policy']];
  var COVERAGE = [['status', 'Coverage status'], ['start_date', 'Coverage start'], ['end_date', 'Coverage end']];
  var LINE = [['service_code', 'Service'], ['service_date', 'Date'], ['modifier', 'Modifier'], ['quantity', 'Qty'], ['unit_price', 'Unit price'], ['net_amount', 'Net'], ['authorization_id', 'Authorization']];
  var NUMERIC = { total_amount: 1, quantity: 1, unit_price: 1, net_amount: 1 };

  function original(claim, path) {
    var seg = path.split('/').slice(1), v = claim;
    for (var i = 0; i < seg.length && v !== undefined && v !== null; i++) v = v[seg[i]];
    return v === undefined ? null : v;
  }
  function toText(v) { return v === null || v === undefined ? '' : String(v); }
  function parse(text, numeric) {
    var t = text.trim();
    if (t === '') return null;
    if (numeric) { var n = Number(t); return isFinite(n) ? n : t; }
    return t;
  }

  CG.fixPanel = function (view, ctx, refresh) {
    var claim = view.claim, inputs = {}, result = h('div'), busy = false, reason;
    function field(path, label, numeric) {
      var input = h('input', { type: 'text', value: toText(original(claim, path)), 'aria-label': label, class: 'fixin' + (numeric ? ' num' : '') });
      input.addEventListener('input', sync);
      inputs[path] = { el: input, numeric: !!numeric };
      return input;
    }
    function changes() {
      var out = [];
      Object.keys(inputs).forEach(function (p) {
        var cur = parse(inputs[p].el.value, inputs[p].numeric), was = original(claim, p);
        if (JSON.stringify(cur) !== JSON.stringify(was)) out.push({ path: p, value: cur });
      });
      return out;
    }
    function sync() {
      Object.keys(inputs).forEach(function (p) {
        var cur = parse(inputs[p].el.value, inputs[p].numeric);
        inputs[p].el.className = 'fixin' + (inputs[p].numeric ? ' num' : '') + (JSON.stringify(cur) !== JSON.stringify(original(claim, p)) ? ' edited' : '');
      });
      var n = changes().length;
      whatBtn.disabled = busy || !n;
      applyBtn.disabled = busy || !n || !reason.value.trim() || !armedOk;
      count.textContent = n ? n + ' field(s) changed' : 'Nothing changed yet';
    }
    var armedOk = false;
    var count = h('span', { class: 'muted small' });
    reason = h('textarea', { rows: 2, maxlength: 1000, placeholder: 'Why this correction (for example: amount corrected from the bill). Do not type the patient or member identifier.' });
    reason.addEventListener('input', sync);

    var headerRows = HEADER.map(function (f) { return h('div', { class: 'fixrow' }, h('label', { text: f[1] }), field('/' + f[0], f[1], !!NUMERIC[f[0]])); });
    var covRows = COVERAGE.map(function (f) { return h('div', { class: 'fixrow' }, h('label', { text: f[1] }), field('/coverage/' + f[0], f[1], false)); });
    var lineTable = h('div', { class: 'tablewrap' }, h('table', null,
      h('thead', null, h('tr', null, [h('th', { text: 'Line' })].concat(LINE.map(function (f) { return h('th', { text: f[1] }); })))),
      h('tbody', null, (claim.lines || []).map(function (l, i) {
        return h('tr', null, [h('td', { class: 'mono', text: l.line_id })].concat(LINE.map(function (f) { return h('td', null, field('/lines/' + i + '/' + f[0], f[1] + ' of ' + l.line_id, !!NUMERIC[f[0]])); })));
      }))));

    function setValue(path, value) {
      var slot = inputs[path];
      if (slot) slot.el.value = toText(value);
    }
    function show(out, stored) {
      CG.clear(result);
      var d = out.diff, cls = d.would_clear ? 'good' : (d.counts.newly_flagged ? 'bad' : 'warn');
      result.appendChild(h('div', { class: 'notice ' + cls },
        h('b', { text: stored ? 'Rechecked as a new version. ' : 'What would happen (nothing is saved): ' }),
        d.would_clear ? 'every check would pass.' : (d.flagged_after + ' finding(s) would remain, ' + d.counts.resolved + ' cleared' + (d.counts.newly_flagged ? ', ' + d.counts.newly_flagged + ' NEW' : '') + '.')));
      var rows = d.rules.filter(function (r) { return r.outcome !== 'unchanged' || r.changed_status; });
      if (rows.length) {
        result.appendChild(CG.table([
          { label: 'Rule', get: function (r) { return h('b', { class: 'mono', text: r.rule_id }); } },
          { label: 'Before', get: function (r) { return CG.chip(r.before); } }, { label: 'After', get: function (r) { return CG.chip(r.after); } },
          { label: 'Result', get: function (r) { var o = OUTCOME[r.outcome]; return CG.chip(o[0], o[1]); } }], rows));
      }
      if (!stored && out.applied) {
        result.appendChild(h('p', { class: 'muted small', text: 'Changes tried: ' + out.applied.map(function (a) { return a.path + ' ' + CG.fmtValue(a.from) + ' to ' + CG.fmtValue(a.to); }).join('; ') }));
      }
      if (!stored && out.copilot && out.copilot.summary && !out.copilot.summary.cleared) {
        result.appendChild(h('p', { class: 'small' }, h('b', { text: 'Then: ' }), out.copilot.summary.headline));
      }
    }
    var whatBtn = h('button', { class: 'btn', type: 'button', text: 'What if? (nothing is saved)', onclick: function () {
      busy = true; sync();
      CG.api('POST', ctx.fixPath('whatif'), { changes: changes() }).then(function (out) { busy = false; sync(); show(out, false); }, function (e) { busy = false; sync(); CG.clear(result); result.appendChild(h('div', { class: 'notice bad', text: detail(e) })); });
    } });
    var applyBtn = h('button', { class: 'btn primary', type: 'button', text: 'Apply the correction and recheck', onclick: function () {
      if (!armedOk) return;
      busy = true; sync();
      CG.api('POST', ctx.fixPath('recheck'), { changes: changes(), reason: reason.value.trim() }).then(function (out) {
        busy = false; show(out, true);
        CG.toast('Saved as version ' + out.new_version + '. ' + (out.still_flagged.length ? out.still_flagged.length + ' finding(s) remain.' : 'No findings remain.'), 'good');
        setTimeout(refresh, 1800);
      }, function (e) { busy = false; sync(); CG.clear(result); result.appendChild(h('div', { class: 'notice bad', text: detail(e) })); });
    } });
    var ack = h('input', { type: 'checkbox', id: 'fixack' });
    ack.addEventListener('change', function () { armedOk = ack.checked; sync(); });

    var card = h('details', { class: 'card fixpanel', open: !!ctx.openFix },
      h('summary', null, h('b', { text: 'Correct and recheck' }), h('span', { class: 'muted small', text: '  change values, see what happens, then save the correction as a new version' })),
      h('p', { class: 'muted small', text: 'The original version is kept exactly as received. A correction is validated again as a new version and goes through triage; identifiers cannot be edited here. ' + (claim.lines && claim.lines.length ? '' : '') }),
      h('div', { class: 'fixgrid' }, headerRows, covRows), h('h3', { text: 'Lines' }), lineTable,
      h('div', { class: 'row', style: null }, whatBtn, count), result,
      h('h3', { text: 'Save it' }), reason,
      h('div', { class: 'row' }, h('label', { class: 'row', for: 'fixack' }, ack, h('span', { text: 'I checked the changed values against the source record.' })), applyBtn));
    sync();
    card.tryEdits = function (edits) {
      card.open = true;
      edits.forEach(function (e) { setValue(e.path, e.to); });
      sync();
      card.scrollIntoView({ behavior: 'smooth', block: 'start' });
      whatBtn.click();
    };
    return card;
  };

  // ---- the request for information the system drafted: the officer confirms that it is sent ---------------------------------
  CG.infoRequestCard = function (view, ctx, refresh) {
    var r = view.info_request;
    if (!r) return null;
    function act(action) {
      CG.api('POST', '/work/claims/' + encodeURIComponent(ctx.id) + '/info-request', { request_id: r.request_id, action: action }).then(function (out) {
        CG.toast(action === 'send' ? 'Request confirmed and placed in the outbox (' + out.decisions_recorded + ' finding(s) recorded as requested).' : 'Request declined. Nothing was sent.', 'good'); refresh();
      }, function (e) { CG.toast(detail(e), 'bad'); });
    }
    var state = r.status === 'sent' ? CG.chip('confirmed: in the outbox', 'good') : (r.status === 'declined' ? CG.chip('declined', 'muted') : CG.chip('drafted by the system, not sent', 'ai'));
    var card = h('div', { class: 'card stack' },
      h('div', { class: 'row between' }, h('h2', { text: 'Request for information' }), state),
      h('p', { class: 'muted small', text: 'The system wrote this message from the findings. Nothing in it is filled in by you; read it, then confirm that it is sent. Confirming also records the request on each finding it covers. This system has no outgoing channel: the confirmed message is put in the outbox for delivery.' }),
      h('div', { class: 'row' }, h('b', { text: 'To: ' }), h('span', { text: 'provider ' + (r.to && r.to.provider_id ? r.to.provider_id : '') })),
      h('div', { class: 'row' }, h('b', { text: 'Subject: ' }), h('span', { text: r.subject })),
      h('pre', { class: 'mono small', text: r.body }));
    if (r.status === 'drafted') {
      card.appendChild(h('div', { class: 'row' },
        h('button', { class: 'btn primary', type: 'button', text: 'Confirm: send this request', onclick: function () { act('send'); } }),
        h('button', { class: 'btn', type: 'button', text: 'Do not send', onclick: function () { act('decline'); } })));
    }
    return card;
  };
})();
