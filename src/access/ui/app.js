/* ClaimGuard console: shell, sign-in and routing. Which pages exist for a person is decided by the permissions the server reports
 * (hide, not disable); the server enforces every one of them again on each request. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  var app = document.getElementById('app');

  var NAV = [
    ['Work', [['#/inbox', 'My inbox', 'claims.decide'], ['#/claims', 'All claims', 'claims.view']]],
    ['Queue', [['#/queue', 'Overview', 'queue.view'], ['#/submit', 'Submit claims', 'routing.manage'], ['#/config', 'Routing settings', 'routing.manage']]],
    ['Administration', [['#/users', 'Users', 'users.manage'], ['#/audit', 'Audit trail', 'audit.view']]]
  ];
  var LEVELS = { 1: 'Viewer', 2: 'Reviewer', 3: 'Senior reviewer', 4: 'Administrator' };

  function firstPage() {
    for (var g = 0; g < NAV.length; g++) for (var i = 0; i < NAV[g][1].length; i++) if (CG.has(NAV[g][1][i][2])) return NAV[g][1][i][0];
    return '#/login';
  }

  // ---- sign-in ----------------------------------------------------------------------------------------------------------
  function loginView() {
    CG.clear(app);
    var badge = h('input', { type: 'text', autocomplete: 'username', required: true, maxlength: '64', autofocus: true });
    var pass = h('input', { type: 'password', autocomplete: 'current-password', required: true, maxlength: '128' });
    var code = h('input', { type: 'text', inputmode: 'numeric', autocomplete: 'one-time-code', pattern: '[0-9]*', maxlength: '8', required: true, placeholder: '6-digit code' });
    var err = h('div', { role: 'alert' });
    var btn = h('button', { class: 'btn primary', type: 'submit', text: 'Sign in' });
    var demo = h('div');
    var form = h('form', { class: 'stack', novalidate: true },
      h('div', { class: 'field' }, h('label', { text: 'Badge number' }), badge),
      h('div', { class: 'field' }, h('label', { text: 'Password' }), pass),
      h('div', { class: 'field' }, h('label', { text: 'Authenticator code' }), code), err, btn);
    function submit(e) {
      if (e) e.preventDefault();
      CG.clear(err); btn.disabled = true;
      CG.api('POST', '/auth/login', { badge_id: badge.value.trim(), password: pass.value, totp: code.value.trim() }).then(function (r) {
        return CG.api('GET', '/auth/me').then(function (me) {
          CG.state.me = me;
          location.hash = r.must_change_password || me.must_change_password ? '#/password' : firstPage();
        });
      }, function (e2) { btn.disabled = false; err.appendChild(h('div', { class: 'notice bad', text: e2.message })); pass.value = ''; code.value = ''; });
    }
    form.addEventListener('submit', submit);
    app.appendChild(h('div', { class: 'login' },
      h('div', { class: 'row', style: null }, h('img', { src: 'icon.svg', alt: '', width: '36', height: '36' }), h('h1', { text: 'ClaimGuard Console' })),
      h('p', { class: 'muted', text: 'Claims review for the people who decide. Sign in with your badge, password and authenticator code.' }),
      h('div', { class: 'card' }, form), demo));
    offerDemo(demo, badge, pass, code, submit);
  }

  // The desktop app's local demo mode exposes pywebview.api.demo_login; it is absent in a browser and on a real server.
  function offerDemo(box, badge, pass, code, submit) {
    function show() {
      var api = window.pywebview && window.pywebview.api;
      if (!api || !api.demo_login || box.firstChild) return;
      box.appendChild(h('div', { class: 'card stack' }, h('b', { text: 'Local demo accounts' }), h('p', { class: 'muted small', text: 'Only in the desktop demo. Fills in a generated sign-in for you.' }),
        h('div', { class: 'row' }, [2, 3, 4, 1].map(function (l) {
          return h('button', { class: 'btn small', type: 'button', text: LEVELS[l], onclick: function () {
            api.demo_login(l).then(function (c) { badge.value = c.badge; pass.value = c.password; code.value = c.code; submit(); });
          } });
        }))));
    }
    show(); window.addEventListener('pywebviewready', show);
  }

  function passwordView() {
    CG.clear(app);
    var oldp = h('input', { type: 'password', autocomplete: 'current-password', required: true, maxlength: '128' });
    var newp = h('input', { type: 'password', autocomplete: 'new-password', required: true, maxlength: '128' });
    var again = h('input', { type: 'password', autocomplete: 'new-password', required: true, maxlength: '128' });
    var err = h('div', { role: 'alert' });
    var form = h('form', { class: 'stack' },
      h('div', { class: 'field' }, h('label', { text: 'Current (temporary) password' }), oldp), h('div', { class: 'field' }, h('label', { text: 'New password' }), newp),
      h('div', { class: 'field' }, h('label', { text: 'New password again' }), again), err, h('button', { class: 'btn primary', type: 'submit', text: 'Set new password' }));
    form.addEventListener('submit', function (e) {
      e.preventDefault(); CG.clear(err);
      if (newp.value !== again.value) { err.appendChild(h('div', { class: 'notice bad', text: 'The two new passwords differ.' })); return; }
      CG.api('POST', '/auth/change-password', { old_password: oldp.value, new_password: newp.value }).then(function () {
        return CG.api('GET', '/auth/me').then(function (me) { CG.state.me = me; CG.toast('Password changed.', 'good'); location.hash = firstPage(); });
      }, function (e2) { err.appendChild(h('div', { class: 'notice bad', text: e2.data && e2.data.detail ? e2.data.detail : e2.message })); });
    });
    app.appendChild(h('div', { class: 'login' }, h('h1', { text: 'Choose a new password' }), h('p', { class: 'muted', text: 'Your account uses a temporary password. Set your own to continue.' }), h('div', { class: 'card' }, form)));
  }

  // ---- shell ------------------------------------------------------------------------------------------------------------
  function shell(active) {
    CG.clear(app);
    var me = CG.state.me;
    var nav = h('nav', { class: 'nav', 'aria-label': 'Main' });
    NAV.forEach(function (g) {
      var items = g[1].filter(function (i) { return CG.has(i[2]); });
      if (!items.length) return;
      nav.appendChild(h('div', { class: 'group', text: g[0] }));
      items.forEach(function (i) { nav.appendChild(h('a', { href: i[0], class: active === i[0] ? 'active' : '', 'aria-current': active === i[0] ? 'page' : null, text: i[1] })); });
    });
    var main = h('main', { class: 'main', id: 'main' });
    app.appendChild(h('div', { class: 'shell' },
      h('aside', { class: 'side' }, h('div', { class: 'brand' }, h('img', { src: 'icon.svg', alt: '' }), h('span', { text: 'ClaimGuard' })), nav,
        h('div', { class: 'me stack' }, h('div', null, h('b', { text: me.name }), h('div', { class: 'muted small', text: me.badge_id + ' - ' + (LEVELS[me.level] || 'Level ' + me.level) })),
          h('button', { class: 'btn small', type: 'button', text: 'Sign out', onclick: signOut }))),
      main));
    return main;
  }

  function signOut() {
    CG.api('POST', '/auth/logout').catch(function () {}).then(function () { CG.state.me = null; CG.stopTimers(); location.hash = '#/login'; });
  }

  // ---- router -----------------------------------------------------------------------------------------------------------
  var ROUTES = [
    [/^#\/inbox$/, 'claims.decide', function (m, el) { CG.views.inbox(el); }, '#/inbox'],
    [/^#\/work\/(.+)$/, 'claims.decide', function (m, el) { CG.views.work(el, decodeURIComponent(m[1])); }, '#/inbox'],
    [/^#\/claims$/, 'claims.view', function (m, el) { CG.views.claims(el); }, '#/claims'],
    [/^#\/claim\/(.+)$/, 'claims.view', function (m, el) { CG.views.claim(el, decodeURIComponent(m[1])); }, '#/claims'],
    [/^#\/queue$/, 'queue.view', function (m, el) { CG.views.queue(el); }, '#/queue'],
    [/^#\/submit$/, 'routing.manage', function (m, el) { CG.views.submit(el); }, '#/submit'],
    [/^#\/config$/, 'routing.manage', function (m, el) { CG.views.config(el); }, '#/config'],
    [/^#\/users$/, 'users.manage', function (m, el) { CG.views.users(el); }, '#/users'],
    [/^#\/audit$/, 'audit.view', function (m, el) { CG.views.audit(el); }, '#/audit']
  ];

  function route() {
    CG.stopTimers();
    var hash = location.hash || '#/';
    if (hash === '#/login') { loginView(); return; }
    if (!CG.state.me) { CG.api('GET', '/auth/me').then(function (me) { CG.state.me = me; route(); }, function () { location.hash = '#/login'; loginView(); }); return; }
    if (CG.state.me.must_change_password || hash === '#/password') { passwordView(); return; }
    for (var i = 0; i < ROUTES.length; i++) {
      var m = ROUTES[i][0].exec(hash);
      if (m) {
        if (!CG.has(ROUTES[i][1])) break;
        var main = shell(ROUTES[i][3]);
        ROUTES[i][2](m, main);
        main.focus && main.setAttribute('tabindex', '-1');
        window.scrollTo(0, 0);
        return;
      }
    }
    var target = firstPage();
    if (target === '#/login') { loginView(); return; }
    if (hash !== target) { location.hash = target; return; }
  }

  window.addEventListener('hashchange', route);
  route();
})();
