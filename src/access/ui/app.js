/* ClaimGuard console: shell, sign-in and routing. Which pages exist for a person is decided by the permissions the server reports
 * (hide, not disable); the server enforces every one of them again on each request. A personal template (CG.prefs) may hide more
 * menu items and pick the home page, but can never show a page the permissions do not allow. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  var app = document.getElementById('app');

  // [route, label, permission (null: every signed-in person), icon]
  var NAV = [
    ['Work', [['#/home', 'Home', null, 'home'], ['#/inbox', 'My inbox', 'claims.decide', 'inbox'], ['#/claims', 'All claims', 'claims.view', 'list'], ['#/ask', 'Ask the rulebook', 'claims.view', 'book']]],
    ['Queue', [['#/queue', 'Overview', 'queue.view', 'grid'], ['#/import', 'Import from files', 'routing.manage', 'upload'], ['#/submit', 'Submit claims', 'routing.manage', 'send'], ['#/batches', 'Batches', 'routing.manage', 'layers'], ['#/config', 'Routing settings', 'routing.manage', 'sliders'], ['#/rules', 'Payer rules', 'routing.manage', 'scale']]],
    ['Administration', [['#/health', 'System health', 'queue.view', 'pulse'], ['#/audit', 'Audit and traceability', 'audit.view', 'shield'], ['#/users', 'Users', 'users.manage', 'users']]],
    ['You', [['#/customize', 'Customize', null, 'palette'], ['#/brand', 'Brand', null, 'star']]]
  ];
  var LEVELS = { 1: 'Viewer', 2: 'Reviewer', 3: 'Senior reviewer', 4: 'Administrator' };
  function allowed(perm) { return perm === null || CG.has(perm); }
  CG.navItems = function () { var all = []; NAV.forEach(function (g) { g[1].forEach(function (i) { if (allowed(i[2])) all.push(i); }); }); return all; };

  function firstPage() {
    if (!CG.state.me) return '#/login';
    var home = CG.prefs.get('home'), items = CG.navItems();
    for (var i = 0; i < items.length; i++) if (items[i][0] === home) return home;
    return items.length ? items[0][0] : '#/login';
  }

  // ---- sign-in ----------------------------------------------------------------------------------------------------------
  // One centred card: the mark, the name and "Claims administration", then the form. Anything passed after the form (the demo staff)
  // sits below the card.
  function authFrame(formSide, below) {
    return h('div', { class: 'auth' },
      h('div', { class: 'auth-col' },
        h('section', { class: 'auth-card' },
          h('div', { class: 'auth-logo' }, h('img', { src: 'icon.svg', alt: '' }), h('b', { text: 'ClaimGuard' }), h('small', { text: 'Claims administration' })),
          formSide),
        below || null,
        h('p', { class: 'synthetic', text: 'All data in this system is synthetic. A passed pre-check is not a payer approval.' })));
  }

  function loginView() {
    CG.clear(app); CG.setTitle('Sign in');
    var badge = h('input', { type: 'text', autocomplete: 'username', required: true, maxlength: '64', autofocus: true, id: 'f-badge' });
    var pass = h('input', { type: 'password', autocomplete: 'current-password', required: true, maxlength: '128', id: 'f-pass' });
    var reveal = h('button', { class: 'btn ghost small', type: 'button', text: 'Show', 'aria-controls': 'f-pass', 'aria-pressed': 'false' });
    reveal.addEventListener('click', function () { var show = pass.type === 'password'; pass.type = show ? 'text' : 'password'; reveal.textContent = show ? 'Hide' : 'Show'; reveal.setAttribute('aria-pressed', String(show)); });
    var code = h('input', { type: 'text', class: 'code-in', inputmode: 'numeric', autocomplete: 'one-time-code', pattern: '[0-9]*', maxlength: '8', required: true, placeholder: '000000', id: 'f-code' });
    var err = h('div', { role: 'alert' });
    var btn = h('button', { class: 'btn primary block', type: 'submit', text: 'Sign in' });
    var demo = h('div');
    var form = h('form', { class: 'stack', novalidate: true },
      h('div', { class: 'field' }, h('label', { for: 'f-badge', text: 'Badge number' }), badge),
      h('div', { class: 'field' }, h('label', { for: 'f-pass', text: 'Password' }), h('div', { class: 'pw' }, pass, reveal)),
      h('div', { class: 'field' }, h('label', { for: 'f-code', text: 'Authenticator code' }, ' ', h('span', { class: 'hint', text: '6 digits from your authenticator app' })), code), err, btn);
    function submit(e) {
      if (e) e.preventDefault();
      CG.clear(err); btn.disabled = true; btn.textContent = 'Signing in...';
      CG.api('POST', '/auth/login', { badge_id: badge.value.trim(), password: pass.value, totp: code.value.trim() }).then(function (r) {
        return CG.api('GET', '/auth/me').then(function (me) {
          CG.state.me = me; CG.prefs.apply();
          location.hash = r.must_change_password || me.must_change_password ? '#/password' : firstPage();
        });
      }, function (e2) { btn.disabled = false; btn.textContent = 'Sign in'; err.appendChild(h('div', { class: 'notice bad', text: e2.message })); pass.value = ''; code.value = ''; });
    }
    form.addEventListener('submit', submit);
    app.appendChild(authFrame(h('div', null, h('h2', { text: 'Sign in' }), form), demo));
    offerDemo(demo, badge, pass, code, submit);
  }

  // Local demo only: the desktop app exposes pywebview.api.demo_login; `serve_access.py --demo` serves GET /demo/login/{level}.
  // Neither exists on a real server (the route answers 404 there), so no demo buttons are shown.
  function offerDemo(box, badge, pass, code, submit) {
    function fetchLogin(l, badgeId) { return fetch('/demo/login/' + l + (badgeId ? '?badge=' + encodeURIComponent(badgeId) : ''), { cache: 'no-store' }).then(function (r) { return r.ok ? r.json() : null; }); }
    function staffCards(people) {
      // one card per demo employee, highest clearance last, each with the level badge the console will show them
      people.sort(function (a, b) { return a.level - b.level || (a.name < b.name ? -1 : 1); });
      box.appendChild(h('div', { class: 'card stack' }, h('b', { text: 'Demo staff' }),
        h('p', { class: 'muted small', text: 'Only in the local demo. Each person opens the console of their clearance level.' }),
        h('div', { class: 'staff' }, people.map(function (p) {
          var role = CG.ROLES[p.level] || CG.ROLES[1];
          return h('button', { class: 'staffcard lvl-' + role.key, type: 'button', onclick: function () {
            fetchLogin(p.level, p.badge).then(function (c) { if (c) { badge.value = c.badge; pass.value = c.password; code.value = c.code; submit(); } });
          } }, CG.avatar(p.name, p.badge),
            h('span', null, h('b', { text: p.name }), h('span', { class: 't', text: (p.title || role.label) + ' · ' + p.badge })), CG.levelBadge(p.level));
        }))));
    }
    function show(viaWeb) {
      var api = window.pywebview && window.pywebview.api;
      var get = api && api.demo_login ? function (l) { return api.demo_login(l); } : (viaWeb ? fetchLogin : null);
      if (!get || box.firstChild) return;
      if (viaWeb) {
        fetch('/demo/people', { cache: 'no-store' }).then(function (r) { return r.ok ? r.json() : null; }).then(function (people) {
          if (box.firstChild) return;
          if (Array.isArray(people) && people.length) staffCards(people); else roleButtons(get);
        }, function () { if (!box.firstChild) roleButtons(get); });
        return;
      }
      roleButtons(get);
    }
    function roleButtons(get) {
      box.appendChild(h('div', { class: 'card stack' }, h('b', { text: 'Local demo accounts' }), h('p', { class: 'muted small', text: 'Only in the local demo. Each role opens its own version of the console.' }),
        h('div', { class: 'row tight' }, [2, 3, 4, 1].map(function (l) {
          return h('button', { class: 'btn small', type: 'button', onclick: function () {
            get(l).then(function (c) { if (c) { badge.value = c.badge; pass.value = c.password; code.value = c.code; submit(); } });
          } }, CG.icon(CG.ROLES[l].icon), LEVELS[l]);
        }))));
    }
    show(false); window.addEventListener('pywebviewready', function () { show(false); });
    // the desktop window always has a (possibly empty) pywebview.api once loaded, so ask for the login bridge itself, not just the object:
    // otherwise the sign-in page drawn again after signing out would show no demo staff
    if (!(window.pywebview && window.pywebview.api && window.pywebview.api.demo_login)) fetchLogin(4).then(function (c) { if (c) show(true); }, function () {});
  }

  function passwordView() {
    CG.clear(app); CG.setTitle('New password');
    var oldp = h('input', { type: 'password', autocomplete: 'current-password', required: true, maxlength: '128', id: 'p-old' });
    var newp = h('input', { type: 'password', autocomplete: 'new-password', required: true, maxlength: '128', id: 'p-new' });
    var again = h('input', { type: 'password', autocomplete: 'new-password', required: true, maxlength: '128', id: 'p-again' });
    var err = h('div', { role: 'alert' });
    var form = h('form', { class: 'stack' },
      h('div', { class: 'field' }, h('label', { for: 'p-old', text: 'Current (temporary) password' }), oldp), h('div', { class: 'field' }, h('label', { for: 'p-new', text: 'New password' }), newp),
      h('div', { class: 'field' }, h('label', { for: 'p-again', text: 'New password again' }), again), err, h('button', { class: 'btn primary block', type: 'submit', text: 'Set new password' }));
    form.addEventListener('submit', function (e) {
      e.preventDefault(); CG.clear(err);
      if (newp.value !== again.value) { err.appendChild(h('div', { class: 'notice bad', text: 'The two new passwords differ.' })); return; }
      CG.api('POST', '/auth/change-password', { old_password: oldp.value, new_password: newp.value }).then(function () {
        return CG.api('GET', '/auth/me').then(function (me) { CG.state.me = me; CG.toast('Password changed.', 'good'); location.hash = firstPage(); });
      }, function (e2) { err.appendChild(h('div', { class: 'notice bad', text: e2.data && e2.data.detail ? e2.data.detail : e2.message })); });
    });
    app.appendChild(authFrame(h('div', null, h('h2', { text: 'Choose a new password' }), h('p', { class: 'muted', text: 'Your account uses a temporary password. Set your own to continue.' }), h('div', { class: 'card' }, form))));
  }

  // ---- shell ------------------------------------------------------------------------------------------------------------
  function shell(active) {
    CG.clear(app);
    var me = CG.state.me, role = CG.ROLES[me.level] || CG.ROLES[2], hidden = CG.prefs.get('hiddenNav') || [];
    var nav = h('nav', { class: 'nav', 'aria-label': 'Main' });
    NAV.forEach(function (g) {
      // a hidden item still shows while it is the open page, so nobody loses their place
      var items = g[1].filter(function (i) { return allowed(i[2]) && (hidden.indexOf(i[0]) < 0 || i[0] === active); });
      if (!items.length) return;
      nav.appendChild(h('div', { class: 'group', text: g[0] }));
      items.forEach(function (i) {
        nav.appendChild(h('a', { href: i[0], title: i[1], class: active === i[0] ? 'active' : '', 'aria-current': active === i[0] ? 'page' : null }, CG.icon(i[3]), h('span', { text: i[1] })));
      });
    });
    var main = h('main', { class: 'main', id: 'main', tabindex: '-1' });
    var side = h('aside', { class: 'side', id: 'side' },
      h('div', { class: 'brand' }, h('img', { src: 'icon-light.svg', alt: '' }), h('span', null, 'ClaimGuard', h('small', { text: 'Claims administration' }))),
      h('div', { class: 'rolebar', title: role.tagline }, CG.icon(role.icon), h('span', { text: role.label + ' workspace' })),
      nav,
      h('div', { class: 'me' },
        h('div', { class: 'who', title: me.name + ' · ' + me.badge_id + ' · clearance level ' + me.level }, avatarPicker(me),
          h('div', null, h('b', { text: me.name }), h('span', { text: me.badge_id + ' · ' + (LEVELS[me.level] || 'Level ' + me.level) }))),
        CG.levelBadge(me.level),
        CG.has('claims.decide') ? CG.notes.bellButton() : null,
        h('div', { class: 'mebtns' },
          h('button', { class: 'btn small railbtn', type: 'button', title: CG.prefs.get('rail') ? 'Expand the menu' : 'Collapse the menu to icons', 'aria-label': CG.prefs.get('rail') ? 'Expand the menu' : 'Collapse the menu to icons', onclick: function () {
            CG.prefs.save({ rail: !CG.prefs.get('rail') }); if (CG.rerender) CG.rerender();
          } }, CG.icon('menu')),
          CG.prefs.get('idbar') ? null : CG.notify.bell(),
          h('button', { class: 'btn small grow', type: 'button', title: 'Sign out', onclick: signOut }, CG.icon('logout'), h('span', { text: 'Sign out' }))),
        h('p', { class: 'synthetic', text: 'Synthetic data only. A passed pre-check is not a payer approval.' })));
    var wrap = h('div', { class: 'shell' });
    var menuBtn = h('button', { class: 'btn small', type: 'button', 'aria-controls': 'side', 'aria-expanded': 'false', 'aria-label': 'Open menu' }, CG.icon('menu'));
    var scrim = null;
    function setMenu(open) {
      wrap.className = 'shell' + (open ? ' menu-open' : ''); menuBtn.setAttribute('aria-expanded', String(open));
      if (open && !scrim) { scrim = h('div', { class: 'scrim', onclick: function () { setMenu(false); } }); wrap.appendChild(scrim); var first = nav.querySelector('a'); if (first) first.focus(); }
      if (!open && scrim) { scrim.remove(); scrim = null; }
    }
    menuBtn.addEventListener('click', function () { setMenu(menuBtn.getAttribute('aria-expanded') !== 'true'); });
    side.addEventListener('keydown', function (e) { if (e.key === 'Escape') { setMenu(false); menuBtn.focus(); } });
    var topbar = h('div', { class: 'topbar' }, h('div', { class: 'brand' }, h('img', { src: 'icon-light.svg', alt: '' }), h('span', null, 'ClaimGuard')), h('div', { class: 'row tight' }, CG.notify.bell(), menuBtn));
    app.appendChild(h('a', { class: 'skip', href: '#main', onclick: function (e) { e.preventDefault(); main.focus(); } , text: 'Skip to content' }));
    wrap.appendChild(side); wrap.appendChild(h('div', { class: 'content' }, topbar, CG.prefs.get('idbar') ? idBar(me, role) : null, main));
    app.appendChild(wrap);
    CG.notify.start();
    return main;
  }

  // The small round picture in the menu: a click opens the file picker; the picture is cropped and kept in this browser for this badge
  function avatarPicker(me) {
    var input = h('input', { type: 'file', accept: 'image/jpeg,image/png,image/webp,image/gif,image/bmp', class: 'sr-only', 'aria-label': 'Change your profile picture' });
    input.addEventListener('change', function () {
      var f = input.files && input.files[0]; input.value = '';
      if (!f) return;
      CG.readImage(f, { max: 256, square: true, quality: 0.86, limit: 300000 }).then(function (url) {
        CG.saveAvatar(me.badge_id, url); CG.toast('Profile picture saved.', 'good'); CG.rerender();
      }, function (e) { CG.toast(e.message, 'bad'); });
    });
    return h('label', { class: 'avatarpick', title: 'Change your profile picture' }, CG.avatar(me.name, me.badge_id), h('span', { class: 'cam', 'aria-hidden': 'true' }, CG.icon('image')), input);
  }

  // The identity bar: on every page, which clearance the person acts under and what that level may do. Viewers see "Read only",
  // supervisors see the separation of duties, so nobody has to guess why a button is missing.
  function idBar(me, role) {
    var note = me.level === 1 ? [CG.icon('lock'), 'Read only'] : me.level === 4 ? [CG.icon('shield'), 'Cannot decide claims'] : me.level === 3 ? [CG.icon('star'), 'High-severity clearance'] : [CG.icon('check'), 'Can decide'];
    return h('div', { class: 'idbar', role: 'note', 'aria-label': 'Your clearance' },
      h('div', { class: 'row' }, CG.levelBadge(me.level), h('span', { class: 'ro' }, note), h('span', { class: 'scope', text: role.scope })),
      h('div', { class: 'row tight' }, CG.notify.bell(), h('a', { href: '#/customize' }, CG.icon('palette'), 'Customize')));
  }

  function signOut() {
    CG.api('POST', '/auth/logout').catch(function () {}).then(function () { CG.state.me = null; CG.notes.stop(); CG.stopTimers(); CG.notify.stop(); CG.prefs.apply(); location.hash = '#/login'; });
  }

  // ---- router -----------------------------------------------------------------------------------------------------------
  var ROUTES = [
    [/^#\/home$/, null, function (m, el) { CG.views.home(el); }, '#/home'],
    [/^#\/customize$/, null, function (m, el) { CG.views.customize(el); }, '#/customize'],
    [/^#\/brand$/, null, function (m, el) { CG.views.brand(el); }, '#/brand'],
    [/^#\/inbox$/, 'claims.decide', function (m, el) { CG.views.inbox(el); }, '#/inbox'],
    [/^#\/work\/(.+)$/, 'claims.decide', function (m, el) { CG.views.work(el, decodeURIComponent(m[1])); }, '#/inbox'],
    [/^#\/claims$/, 'claims.view', function (m, el) { CG.views.claims(el); }, '#/claims'],
    [/^#\/claim\/(.+)$/, 'claims.view', function (m, el) { CG.views.claim(el, decodeURIComponent(m[1])); }, '#/claims'],
    [/^#\/queue$/, 'queue.view', function (m, el) { CG.views.queue(el); }, '#/queue'],
    [/^#\/import$/, 'routing.manage', function (m, el) { CG.views.importClaims(el); }, '#/import'],
    [/^#\/submit$/, 'routing.manage', function (m, el) { CG.views.submit(el); }, '#/submit'],
    [/^#\/batches$/, 'routing.manage', function (m, el) { CG.views.batches(el); }, '#/batches'],
    [/^#\/rules$/, 'routing.manage', function (m, el) { CG.views.rules(el); }, '#/rules'],
    [/^#\/ask$/, 'claims.view', function (m, el) { CG.views.ask(el); }, '#/ask'],
    [/^#\/config$/, 'routing.manage', function (m, el) { CG.views.config(el); }, '#/config'],
    [/^#\/users$/, 'users.manage', function (m, el) { CG.views.users(el); }, '#/users'],
    [/^#\/health$/, 'queue.view', function (m, el) { CG.views.health(el); }, '#/health'],
    [/^#\/audit$/, 'audit.view', function (m, el) { CG.views.audit(el); }, '#/audit'],
    [/^#\/trace\/(.+)$/, 'audit.view', function (m, el) { CG.views.trace(el, decodeURIComponent(m[1])); }, '#/audit']
  ];
  var TITLES = {}; NAV.forEach(function (g) { g[1].forEach(function (i) { TITLES[i[0]] = i[1]; }); });

  function route() {
    CG.stopTimers();
    var hash = location.hash || '#/';
    if (hash === '#/login') { CG.notes.stop(); loginView(); return; }
    if (!CG.state.me) { CG.api('GET', '/auth/me').then(function (me) { CG.state.me = me; CG.prefs.apply(); route(); }, function () { location.hash = '#/login'; loginView(); }); return; }
    if (CG.state.me.must_change_password || hash === '#/password') { passwordView(); return; }
    for (var i = 0; i < ROUTES.length; i++) {
      var m = ROUTES[i][0].exec(hash);
      if (m) {
        if (!allowed(ROUTES[i][1])) break;
        CG.notes.start();
        var main = shell(ROUTES[i][3]);
        CG.setTitle(TITLES[ROUTES[i][3]]);
        ROUTES[i][2](m, main);
        window.scrollTo(0, 0);
        if (CG.state.navigated) main.focus({ preventScroll: true });   // move keyboard and screen-reader focus to the new page, not on first load
        CG.state.navigated = true;
        return;
      }
    }
    var target = firstPage();
    if (target === '#/login') { loginView(); return; }
    if (hash !== target) { location.hash = target; return; }
  }

  CG.rerender = function () { var y = window.scrollY; CG.state.navigated = false; route(); window.scrollTo(0, y); };   // a template change that alters the shell (menu items)
  CG.prefs.apply();
  window.addEventListener('hashchange', route);
  route();
})();
