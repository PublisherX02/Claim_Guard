/* ClaimGuard console: import claims from files.
 * A PDF, a photo or scan, a FHIR file, a JSON/JSONL file or the five CSV tables go in; what comes out is a PREVIEW. Nothing is queued until a person
 * has looked at it. A claim read from a document is only a draft: every field shows where it came from and what is doubtful about it, can be edited,
 * and is sent only after the person confirms they compared it with the page. Text from the server is untrusted and goes on the page as text only. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  var SCALARS = [['claim_id', 'Claim ID'], ['invoice_number', 'Invoice number'], ['patient_id', 'Patient ID'], ['member_id', 'Member ID'], ['provider_id', 'Provider'],
    ['payer_id', 'Payer'], ['policy_id', 'Policy'], ['diagnosis_code', 'Diagnosis code'], ['submission_date', 'Submission date (YYYY-MM-DD)'], ['currency', 'Currency'], ['total_amount', 'Total amount']];
  var COVERAGE = [['status', 'Coverage status'], ['start_date', 'Coverage starts'], ['end_date', 'Coverage ends']];
  var LINE_COLS = [['line_id', 'Line'], ['service_code', 'Service'], ['service_date', 'Date'], ['modifier', 'Mod'], ['quantity', 'Qty'], ['unit_price', 'Unit price'], ['net_amount', 'Net amount'], ['authorization_id', 'Authorization']];
  var NUMERIC = { total_amount: true, quantity: true, unit_price: true, net_amount: true };
  var CSV_NAMES = ['claims', 'lines', 'coverage', 'authorizations', 'attachments'];
  var NOTE_FLAGS = ['disagree', 'NOT confirmed', 'unsure', 'ambiguous', 'not a real date', 'not found'];

  function why(e) {
    var d = e && e.data;
    var text = d && (d.reason || d.detail);
    if (d && d.error === 'document_reader_not_configured') return 'This server has no document reader, so PDFs and pictures cannot be read here. JSON, FHIR and CSV files still can.';
    if (d && d.error === 'refused') return 'The file was refused: ' + (d.reason || d.code);
    return text ? (e.message + ' ' + text) : (e && e.message) || 'Something went wrong.';
  }

  function toValue(key, text, numeric) {
    var t = String(text).trim();
    if (t === '') return null;
    if (numeric && /^-?\d+(\.\d+)?$/.test(t)) return Number(t);
    return t;
  }

  function show(v) { return v === null || v === undefined ? '' : String(v); }

  // the evidence for one field: what the page said, how sure the reader was, and what is doubtful about it
  function evidenceFor(path, meta, warnings) {
    var out = [], flagged = false;
    (meta[path] || []).forEach(function (f) {
      if (f.evidence) out.push(h('div', { class: 'muted small', text: 'From: ' + f.evidence }));
      (f.notes || []).forEach(function (n) { out.push(h('div', { class: 'small flagtext', text: n })); if (NOTE_FLAGS.some(function (k) { return n.indexOf(k) >= 0; })) flagged = true; });
      if (f.low_confidence) flagged = true;
    });
    warnings.forEach(function (w) {
      if (w.indexOf(path) === 0) { out.push(h('div', { class: 'small flagtext', text: w })); flagged = true; }
    });
    return { nodes: out, flagged: flagged };
  }

  function claimEditor(draft, onChange) {
    var claim = draft.claim, inputs = {}, meta = {};
    (draft.fields || []).forEach(function (f) { (meta[f.path] = meta[f.path] || []).push(f); });
    var warnings = draft.warnings || [];
    function field(path, label, value, numeric, listen) {
      var inp = h('input', { type: 'text', value: show(value), 'aria-label': label, autocomplete: 'off', spellcheck: 'false' });
      var ev = evidenceFor(path, meta, warnings);
      var empty = show(value) === '';
      if (ev.flagged || empty) inp.className = 'flagged';
      inp.addEventListener('input', function () { inp.className = ''; onChange(); });
      inputs[path] = { el: inp, numeric: numeric };
      return h('div', { class: 'field' }, h('label', { text: label }), inp, ev.nodes, empty && !ev.nodes.length ? h('div', { class: 'small flagtext', text: 'Not found in the document: left empty, never guessed.' }) : null);
    }
    var grid = h('div', { class: 'importgrid' });
    SCALARS.forEach(function (s) { grid.appendChild(field(s[0], s[1], claim[s[0]], !!NUMERIC[s[0]])); });
    COVERAGE.forEach(function (s) { grid.appendChild(field('coverage.' + s[0], s[1], (claim.coverage || {})[s[0]], false)); });
    // lines
    var linesBody = h('tbody'), lineRows = [];
    function addLine(line, idx) {
      var cells = {};
      var tr = h('tr', null, LINE_COLS.map(function (c) {
        var path = 'lines[' + idx + '].' + c[0];
        var ev = evidenceFor(path, meta, warnings.concat(warnings.filter(function (w) { return w.indexOf('lines[' + idx + ']:') === 0; })));
        var empty = show(line[c[0]]) === '';
        var inp = h('input', { type: 'text', value: show(line[c[0]]), 'aria-label': c[1] + ' of line ' + (idx + 1), autocomplete: 'off', spellcheck: 'false', class: (ev.flagged || (empty && c[0] !== 'modifier' && c[0] !== 'authorization_id')) ? 'flagged' : '' });
        inp.addEventListener('input', function () { inp.className = ''; onChange(); });
        cells[c[0]] = inp;
        return h('td', null, inp, ev.nodes.length ? h('details', { class: 'small' }, h('summary', { text: 'why' }), ev.nodes) : null);
      }));
      var del = h('button', { class: 'btn small', type: 'button', text: 'Remove', onclick: function () { tr.remove(); lineRows = lineRows.filter(function (r) { return r.tr !== tr; }); onChange(); } });
      tr.appendChild(h('td', null, del));
      lineRows.push({ tr: tr, cells: cells, original: line });
      linesBody.appendChild(tr);
    }
    (claim.lines || []).forEach(addLine);
    var table = h('div', { class: 'tablewrap' }, h('table', null, h('thead', null, h('tr', null, LINE_COLS.map(function (c) { return h('th', { text: c[1] }); }).concat([h('th')]))), linesBody));
    var addBtn = h('button', { class: 'btn small', type: 'button', text: 'Add a line', onclick: function () { addLine({ line_id: 'L' + (lineRows.length + 1) }, lineRows.length); onChange(); } });
    var attachments = (claim.attachments || []).slice();
    var attBox = h('div');
    function drawAttachments() {
      CG.clear(attBox);
      if (!attachments.length) return;
      attBox.appendChild(h('h3', { text: 'Attached documents' }));
      attBox.appendChild(CG.table([{ label: 'Document', get: function (a) { return h('span', { class: 'mono', text: a.attachment_id }); } }, { label: 'Type', get: function (a) { return a.type; } }, { label: 'Patient', get: function (a) { return a.patient_id; } },
        { label: 'Service', get: function (a) { return a.service_code; } }, { label: 'Date', get: function (a) { return a.service_date; } }, { label: 'Status', get: function (a) { return a.document_status; } },
        { label: '', get: function (a) { return h('button', { class: 'btn small', type: 'button', text: 'Remove', onclick: function () { attachments = attachments.filter(function (x) { return x !== a; }); drawAttachments(); onChange(); } }); } }], attachments));
    }
    drawAttachments();
    var node = h('div', { class: 'stack' }, grid, h('h3', { text: 'Lines' }), table, addBtn, attBox);
    return {
      node: node,
      addAttachment: function (a) { attachments.push(a); drawAttachments(); onChange(); },
      build: function () {
        var c = JSON.parse(JSON.stringify(claim));
        SCALARS.forEach(function (s) { c[s[0]] = toValue(s[0], inputs[s[0]].el.value, !!NUMERIC[s[0]]); });
        c.coverage = c.coverage || {};
        COVERAGE.forEach(function (s) { c.coverage[s[0]] = toValue(s[0], inputs['coverage.' + s[0]].el.value, false); });
        c.lines = lineRows.map(function (r) {
          var l = {};
          LINE_COLS.forEach(function (col) { l[col[0]] = toValue(col[0], r.cells[col[0]].value, !!NUMERIC[col[0]]); });
          return l;
        });
        var known = {}; (c.authorizations || []).forEach(function (a) { known[a.authorization_id] = a; });
        var seen = {};
        c.authorizations = [];
        c.lines.forEach(function (l) {
          var id = l.authorization_id;
          if (id && !seen[id]) { seen[id] = true; c.authorizations.push(known[id] || { authorization_id: id, patient_id: null, service_code: null, status: null, valid_from: null, valid_to: null, max_quantity: null }); }
        });
        c.attachments = attachments;
        return c;
      }
    };
  }

  function sendClaims(claims, out, done) {
    var rows = [], i = 0, progress = h('p', { class: 'muted' });
    out.appendChild(progress);
    (function next() {
      if (i >= claims.length) {
        progress.remove();
        var ok = rows.filter(function (r) { return r.accepted; }).length;
        out.appendChild(h('div', { class: 'notice ' + (ok === rows.length ? 'good' : 'warn'), text: ok + ' of ' + rows.length + ' claims accepted into the queue. Workers pick them up within seconds.' }));
        var bad = rows.filter(function (r) { return !r.accepted; });
        if (bad.length) out.appendChild(CG.table([{ label: 'Claim', get: function (r) { return r.claim_id; } }, { label: 'Why it was refused', get: function (r) { return r.reason; } }], bad));
        if (done) done(rows);
        return;
      }
      var batch = claims.slice(i, i + 20); i += 20;
      progress.textContent = 'Sending ' + Math.min(i, claims.length) + ' of ' + claims.length + '...';
      CG.api('POST', '/queue/submit', { claims: batch }).then(function (r) { rows = rows.concat(r.results); next(); }, function (e) { progress.remove(); CG.fail(e); if (done) done(rows); });
    })();
  }

  function recordsCard(name, res) {
    var items = res.items || [], checks = [];
    var sum = res.summary || {};
    var card = h('div', { class: 'card stack' }, h('h2', { text: name }),
      h('p', { class: 'muted', text: 'Read as ' + String((res.source || {}).format).replace(/_/g, ' ') + ': ' + items.length + ' record(s), ' + (sum.accepted || 0) + ' acceptable, ' + (sum.quarantined || 0) + ' refused. Nothing has been queued.' }));
    var rows = items.map(function (it, i) {
      var cb = h('input', { type: 'checkbox', 'aria-label': 'Send ' + (it.ref || i), checked: !!it.accepted, disabled: !it.accepted });
      if (it.accepted) cb.checked = true;
      checks.push({ cb: cb, item: it });
      return { it: it, cb: cb };
    });
    card.appendChild(CG.table([
      { label: 'Send', get: function (r) { return r.cb; } },
      { label: 'Record', get: function (r) { return h('span', { class: 'mono', text: r.it.ref }); } },
      { label: 'Claim', get: function (r) { return r.it.claim ? r.it.claim.claim_id : null; } },
      { label: 'Result', get: function (r) { return r.it.accepted ? CG.chip('acceptable', 'green') : CG.chip('refused', 'high'); } },
      { label: 'Why / notes', get: function (r) { return r.it.accepted ? ((r.it.warnings || []).length ? (r.it.warnings.length + ' note(s): ' + r.it.warnings[0]) : null) : r.it.error; } }
    ], rows));
    var out = h('div'), go = h('button', { class: 'btn primary', type: 'button', text: 'Send the checked claims to the queue' });
    go.addEventListener('click', function () {
      var claims = checks.filter(function (c) { return c.cb.checked && c.item.accepted; }).map(function (c) { return c.item.claim; });
      if (!claims.length) { CG.toast('No claim is checked.', 'warn'); return; }
      go.disabled = true; CG.clear(out);
      sendClaims(claims, out, function () { go.disabled = false; });
    });
    if (items.some(function (i) { return i.accepted; })) card.appendChild(go);
    card.appendChild(out);
    return card;
  }

  function documentCard(name, res, file, ctx) {
    var d = res.draft, src = res.source || {}, reader = res.reader || {};
    var card = h('div', { class: 'card stack' }, h('h2', { text: name }));
    var pagesInfo = (reader.pages || []).map(function (p) { return 'page ' + p.page + ': ' + (p.source === 'text_layer' ? 'text layer of the PDF' : 'read from the picture' + (p.second_reading ? ' by two readers' : '')); }).join('; ');
    var left = h('div', { class: 'stack' });
    if (/^image\//.test(file.type)) left.appendChild(h('img', { class: 'preview', src: URL.createObjectURL(file), alt: 'The uploaded page, to compare with the fields' }));
    else left.appendChild(h('div', { class: 'notice info', text: 'PDF, ' + (reader.pages || []).length + ' page(s). The browser does not draw it here; compare the fields with the file itself.' }));
    left.appendChild(h('p', { class: 'muted small', text: 'Reader: ' + (reader.engine || '?') + '. ' + pagesInfo + '. File hash ' + String(src.sha256 || '').slice(0, 16) + '...' }));
    var right = h('div', { class: 'stack' });
    card.appendChild(h('div', { class: 'importdoc' }, left, right));
    if (d.kind === 'attachment') {
      var a = d.attachment;
      right.appendChild(h('div', { class: 'notice info', text: 'This looks like a supporting document, not a claim form. It can be attached to a claim draft on this page; its text is kept as untrusted data.' }));
      right.appendChild(CG.kv([['Type', a.type], ['Patient', a.patient_id], ['Service', a.service_code], ['Date', a.service_date], ['Status', a.document_status], ['Document id', a.attachment_id]]));
      var pick = h('select', { 'aria-label': 'Attach to claim draft' });
      var attach = h('button', { class: 'btn', type: 'button', text: 'Attach to the chosen claim draft' });
      function refresh() { CG.clear(pick); ctx.claims.forEach(function (c, i) { pick.appendChild(h('option', { value: String(i), text: (c.label() || 'draft ' + (i + 1)) })); }); attach.disabled = !ctx.claims.length; }
      attach.addEventListener('click', function () { var c = ctx.claims[Number(pick.value)]; if (c) { c.editor.addAttachment(a); CG.toast('Attached to ' + c.label() + '.', 'good'); } });
      ctx.onClaimDrafts.push(refresh); refresh();
      right.appendChild(h('div', { class: 'row' }, pick, attach));
      return card;
    }
    var status = h('div'), confirm = h('input', { type: 'checkbox', id: 'cf-' + Math.random().toString(36).slice(2) });
    var checkBtn = h('button', { class: 'btn', type: 'button', text: 'Check this claim' });
    var send = h('button', { class: 'btn primary', type: 'button', text: 'Send to the queue', disabled: true });
    var out = h('div'), checked = false;
    var editor;
    function changed() { checked = false; send.disabled = true; CG.clear(status); }
    editor = claimEditor(d, changed);
    ctx.claims.push({ editor: editor, label: function () { return (editor.build().claim_id || 'unnamed draft'); } });
    ctx.onClaimDrafts.forEach(function (f) { f(); });
    right.appendChild(h('div', { class: 'notice warn', text: 'A DRAFT, not a claim yet. A reader can misread a character, and OCR cannot be trusted with an identifier: compare every field with the page. Fields in orange are empty or doubtful.' }));
    if ((d.warnings || []).length) right.appendChild(h('details', null, h('summary', { text: (d.warnings.length) + ' note(s) about this reading' }), h('ul', { class: 'plain' }, d.warnings.map(function (w) { return h('li', { class: 'small', text: w }); }))));
    right.appendChild(editor.node);
    checkBtn.addEventListener('click', function () {
      CG.clear(status);
      CG.api('POST', '/queue/import/validate', { claim: editor.build() }).then(function (r) {
        checked = !!r.valid;
        status.appendChild(r.valid ? h('div', { class: 'notice good', text: 'The claim passes the format check. Tick the box and send it.' }) : h('div', { class: 'notice bad', text: 'Not acceptable yet: ' + r.error + '.' }));
        send.disabled = !(checked && confirm.checked);
      }, CG.fail);
    });
    confirm.addEventListener('change', function () { send.disabled = !(checked && confirm.checked); });
    send.addEventListener('click', function () {
      send.disabled = true; CG.clear(out);
      sendClaims([editor.build()], out, function (rows) { if (rows.length && rows[0].accepted) { checkBtn.disabled = true; confirm.disabled = true; } else { send.disabled = false; } });
    });
    CG.append(right, [h('div', { class: 'row' }, checkBtn, h('label', { class: 'row small', for: confirm.id }, confirm, ' I compared every field with the page')), status, send, out]);
    return card;
  }

  CG.views.importClaims = function (root) {
    var ctx = { claims: [], onClaimDrafts: [] };
    var input = h('input', { type: 'file', multiple: true, accept: '.pdf,.png,.jpg,.jpeg,.json,.jsonl,.txt,.csv,application/pdf,image/png,image/jpeg,application/json,text/csv', 'aria-label': 'Files to import' });
    var go = h('button', { class: 'btn primary', type: 'button', text: 'Read the file(s)' });
    var results = h('div', { class: 'stack' });
    go.addEventListener('click', function () {
      var files = Array.prototype.slice.call(input.files || []);
      if (!files.length) { CG.toast('Choose a file first.', 'warn'); return; }
      go.disabled = true; CG.clear(results); ctx.claims = []; ctx.onClaimDrafts = [];
      var csv = {}, others = [];
      files.forEach(function (f) { var base = f.name.toLowerCase().replace(/\.csv$/, ''); if (/\.csv$/i.test(f.name) && CSV_NAMES.indexOf(base) >= 0) csv[base] = f; else others.push(f); });
      var jobs = others.map(function (f) { return { name: f.name, run: function () { return f.arrayBuffer().then(function (buf) { return CG.upload('/queue/import/file', f.name, buf, f.type || 'application/octet-stream'); }); }, file: f }; });
      if (Object.keys(csv).length) {
        var missing = CSV_NAMES.filter(function (n) { return !csv[n]; });
        if (missing.length) results.appendChild(h('div', { class: 'notice bad', text: 'The CSV export is five tables; these are missing: ' + missing.join(', ') + '.' }));
        else jobs.push({ name: 'CSV tables (' + CSV_NAMES.join(', ') + ')', run: function () { return Promise.all(CSV_NAMES.map(function (n) { return csv[n].text(); })).then(function (texts) { var body = {}; CSV_NAMES.forEach(function (n, i) { body[n] = texts[i]; }); return CG.api('POST', '/queue/import/csv', body); }); } });
      }
      var i = 0;
      (function next() {
        if (i >= jobs.length) { go.disabled = false; return; }
        var job = jobs[i++], slot = h('div', { class: 'card' }, h('p', { class: 'muted', text: 'Reading ' + job.name + '...' }));
        results.appendChild(slot);
        job.run().then(function (res) {
          var card = res.kind === 'document' ? documentCard(job.name, res, job.file, ctx) : recordsCard(job.name, res);
          slot.replaceWith(card);
        }, function (e) { slot.replaceWith(h('div', { class: 'notice bad' }, h('b', { text: job.name + ': ' }), why(e))); }).then(next);
      })();
    });
    root.appendChild(h('h1', { text: 'Import claims from files' }));
    root.appendChild(h('div', { class: 'card stack' },
      h('p', { class: 'muted', text: 'Claims do not always arrive as JSON. Choose a scanned or photographed claim (PNG, JPEG), a PDF, a FHIR bundle file, a JSON or JSONL file, or the five CSV tables of the export (select all five at once). The system reads them and shows what it would accept, field by field. Nothing is queued until you confirm. Pictures are read by two independent readers and any field they disagree on is flagged; a document you upload is checked for hidden or active content first.' }),
      h('div', { class: 'field' }, h('label', { text: 'Files' }), input), go));
    root.appendChild(results);
  };
})();
