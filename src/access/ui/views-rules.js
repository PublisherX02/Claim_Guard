/* ClaimGuard console: payer rules. The numbers and lists that the fifteen rules compare a claim with (price and quantity limits, allowed providers, services that
 * need authorization, documents each service requires) are data an administrator can change; the rules themselves are code and cannot be changed here. A change is
 * checked, shown as a list of differences, needs a reason, and (with two or more administrators) waits for a different administrator to approve it. Every version
 * is kept in a tamper-evident history. Text from the server is untrusted and goes on the page as text only. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;

  function lines(text) { return String(text).split('\n').map(function (l) { return l.trim(); }).filter(Boolean); }
  function pairs(text, numeric) {
    var out = {};
    lines(text).forEach(function (l) {
      var m = /^([^:=\s]+)\s*[:=]\s*(.+)$/.exec(l);
      if (!m) throw new Error('"' + l + '" is not of the form SERVICE: value');
      var v = m[2].trim();
      if (numeric) { if (!/^-?\d+(\.\d+)?$/.test(v)) throw new Error('"' + v + '" is not a number'); out[m[1]] = Number(v); } else out[m[1]] = v;
    });
    return out;
  }
  function showPairs(o) { return Object.keys(o || {}).sort().map(function (k) { return k + ': ' + o[k]; }).join('\n'); }

  CG.views.rules = function (root) {
    var body = h('div', { class: 'stack' }), out = h('div'), history = h('div');
    var current = null, editors = {};

    function build() {
      var table = JSON.parse(JSON.stringify(current.policies));
      Object.keys(editors).forEach(function (id) {
        var e = editors[id], p = table[id];
        p.currency = e.currency.value.trim();
        p.submission_window_days = /^-?\d+$/.test(e.window.value.trim()) ? Number(e.window.value.trim()) : e.window.value.trim();
        p.allowed_providers = lines(e.providers.value);
        p.auth_required_services = lines(e.auth.value);
        p.required_documents = pairs(e.docs.value, false);
        p.max_unit_price = pairs(e.price.value, true);
        p.max_quantity_per_line = pairs(e.qty.value, true);
      });
      return table;
    }

    function draw() {
      CG.clear(body); CG.clear(history); editors = {};
      body.appendChild(h('div', { class: 'notice info' }, h('b', { text: 'Version ' + current.version + ' is in force. ' }),
        'The fifteen rules are fixed. What a payer contract says is the table below. A change needs a reason, is shown as a list of differences first, and with two or more administrators a different administrator must approve it.'));
      Object.keys(current.policies).sort().forEach(function (id) {
        var p = current.policies[id];
        var e = {
          currency: h('input', { type: 'text', value: p.currency, maxlength: '3', 'aria-label': id + ' currency' }),
          window: h('input', { type: 'text', value: String(p.submission_window_days), inputmode: 'numeric', 'aria-label': id + ' submission window in days' }),
          providers: h('textarea', { rows: '3', 'aria-label': id + ' allowed providers', value: (p.allowed_providers || []).join('\n') }),
          auth: h('textarea', { rows: '3', 'aria-label': id + ' services that need authorization', value: (p.auth_required_services || []).join('\n') }),
          docs: h('textarea', { rows: '3', 'aria-label': id + ' required documents', value: showPairs(p.required_documents) }),
          price: h('textarea', { rows: '6', 'aria-label': id + ' maximum unit price', value: showPairs(p.max_unit_price) }),
          qty: h('textarea', { rows: '6', 'aria-label': id + ' maximum quantity per line', value: showPairs(p.max_quantity_per_line) })
        };
        editors[id] = e;
        body.appendChild(h('div', { class: 'card stack policycard' }, h('h2', null, h('span', { class: 'rid', text: id }), p.payer_id ? ' payer ' + p.payer_id : null),
          h('div', { class: 'importgrid' },
            h('div', { class: 'field' }, h('label', { text: 'Currency' }), e.currency),
            h('div', { class: 'field' }, h('label', { text: 'Days allowed to submit' }), e.window)),
          h('div', { class: 'importgrid' },
            h('div', { class: 'field' }, h('label', { text: 'Allowed providers (one per line)' }), e.providers),
            h('div', { class: 'field' }, h('label', { text: 'Services that need authorization (one per line)' }), e.auth),
            h('div', { class: 'field' }, h('label', { text: 'Document a service requires (SERVICE: type)' }), e.docs)),
          h('div', { class: 'importgrid' },
            h('div', { class: 'field' }, h('label', { text: 'Highest unit price (SERVICE: amount)' }), e.price),
            h('div', { class: 'field' }, h('label', { text: 'Highest quantity per line (SERVICE: number)' }), e.qty))));
      });
      var reason = h('textarea', { rows: '2', maxlength: '300', placeholder: 'Why does the contract change? (10 to 300 characters; kept in the history)', 'aria-label': 'Reason for the change' });
      var review = h('button', { class: 'btn', type: 'button', text: 'Review the change' });
      var send = h('button', { class: 'btn primary', type: 'button', text: 'Submit the change', disabled: true });
      var verdict = h('div'), reviewed = null;
      function stale() { reviewed = null; send.disabled = true; }
      body.addEventListener('input', stale);
      review.addEventListener('click', function () {
        var table;
        CG.clear(verdict);
        try { table = build(); } catch (err) { verdict.appendChild(h('div', { class: 'notice bad', text: err.message })); return; }
        CG.api('POST', '/rules/payer/validate', { policies: table }).then(function (v) {
          if (v.errors.length) verdict.appendChild(h('div', { class: 'notice bad' }, h('b', { text: 'Not acceptable: ' }), h('ul', { class: 'plain' }, v.errors.map(function (x) { return h('li', { text: x }); }))));
          if (v.warnings.length) verdict.appendChild(h('div', { class: 'notice warn' }, h('ul', { class: 'plain' }, v.warnings.map(function (x) { return h('li', { text: x }); }))));
          if (v.changes.length) verdict.appendChild(h('div', { class: 'card' }, h('h3', { text: 'What would change (' + v.changes.length + ')' }), h('ul', { class: 'plain' }, v.changes.map(function (x) { return h('li', { class: 'mono small', text: x }); }))));
          if (v.ok && v.changes.length) { reviewed = table; send.disabled = false; }
        }, CG.fail);
      });
      send.addEventListener('click', function () {
        if (!reviewed) return;
        if (reason.value.trim().length < 10) { CG.toast('Write the reason first (at least 10 characters).', 'warn'); return; }
        send.disabled = true;
        CG.api('POST', '/rules/payer', { policies: reviewed, reason: reason.value.trim() }).then(function (r) {
          CG.clear(out);
          if (r.status === 'pending_approval') out.appendChild(h('div', { class: 'notice warn', text: 'Nothing has changed yet: the change is waiting for a second administrator to approve it (request ' + r.approval.id + '). They find it under Users, "Waiting for a second administrator".' }));
          else { out.appendChild(h('div', { class: 'notice good', text: 'Applied as version ' + r.version + '. Claims sent from now on are judged under it; the receipt of each says which table it used.' })); load(); }
        }, function (err) { send.disabled = false; CG.toast((err.data && err.data.detail) || err.message, 'bad'); });
      });
      body.appendChild(h('div', { class: 'card stack' }, h('div', { class: 'field' }, h('label', { text: 'Reason' }), reason), h('div', { class: 'row' }, review, send), verdict));
      history.appendChild(h('h2', { class: 'sect', text: 'History' }));
      history.appendChild(h('div', { class: 'card' }, CG.table([
        { label: 'Version', num: true, get: function (r) { return r.version; } },
        { label: 'When', get: function (r) { return r.time; } },
        { label: 'Asked by', get: function (r) { return r.actor; } },
        { label: 'Approved by', get: function (r) { return r.approver; } },
        { label: 'Reason', get: function (r) { return r.reason; } },
        { label: 'Changes', get: function (r) { return (r.changes || []).length ? h('details', null, h('summary', { text: r.changes.length + ' change(s)' }), h('ul', { class: 'plain' }, r.changes.map(function (c) { return h('li', { class: 'mono small', text: c }); }))) : null; } }
      ], current.history.slice().reverse())));
    }

    function load() {
      CG.api('GET', '/rules/payer').then(function (r) { current = r; draw(); }, CG.fail);
    }
    root.appendChild(h('h1', { text: 'Payer rules' }));
    root.appendChild(out); root.appendChild(body); root.appendChild(history);
    load();
  };
})();
