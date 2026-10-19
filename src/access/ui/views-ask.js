/* ClaimGuard console: ask the rulebook. A question is matched against the text of the fifteen rules and the payer policies in force, by meaning when the
 * search service is running and by keywords otherwise; the answer is the quoted
 * passage and where it comes from. Nothing is generated, so nothing is invented, and a question the documents cannot answer is refused. It never reads a claim.
 * Text from the server is untrusted and goes on the page as text only. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;

  CG.views.ask = function (root) {
    var q = h('input', { type: 'text', maxlength: '300', placeholder: 'For example: which services need an authorization reference?', 'aria-label': 'Your question' });
    var go = h('button', { class: 'btn primary', type: 'button', text: 'Search the rulebook' });
    var out = h('div', { class: 'stack' });
    function run() {
      var text = q.value.trim();
      if (!text) return;
      CG.clear(out);
      CG.api('GET', '/knowledge/search?q=' + encodeURIComponent(text)).then(function (r) {
        out.appendChild(h('div', { class: r.answered ? 'notice info' : 'notice warn', text: r.note + (r.answered ? ' Policy table version ' + r.table_version + '.' : '') }));
        if (r.fallback) out.appendChild(h('div', { class: 'notice warn', text: r.fallback }));
        if (r.answered && r.caveat) out.appendChild(h('div', { class: 'small', text: 'Search by meaning: ' + r.caveat + '.' }));
        r.results.forEach(function (x) {
          out.appendChild(h('div', { class: 'card stack' }, h('div', { class: 'row' }, h('b', { text: x.source }), h('span', { class: 'small', text: x.kind })), h('p', { text: x.text })));
        });
      }, CG.fail);
    }
    go.addEventListener('click', run);
    q.addEventListener('keydown', function (e) { if (e.key === 'Enter') run(); });
    root.appendChild(h('h1', { text: 'Ask the rulebook' }));
    root.appendChild(h('div', { class: 'notice info', text: 'This searches the text of the fifteen rules and the payer policies now in force. It quotes them with their source; it does not write answers, give clinical advice or look at any claim.' }));
    root.appendChild(h('div', { class: 'card stack' }, h('div', { class: 'field' }, h('label', { text: 'Question' }), q), h('div', { class: 'row' }, go)));
    root.appendChild(out);
  };
})();
