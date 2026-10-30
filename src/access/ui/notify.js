/* ClaimGuard notification centre (docs/62 section 9): tells the person when new claims arrive in the queue, and when claims are routed to
 * their own inbox. Each alert plays a short sound made with the Web Audio API (no file to download, nothing for the CSP to allow) and,
 * when the tab is in the background and the person opted in, shows a desktop notification.
 *
 * Sources, all read-only and polled every few seconds while someone is signed in:
 *   - the demo feed (GET /demo/feed): the demo server's intake log, from this PC only; the "Simulate" button posts to /demo/arrivals
 *   - outside the demo, for whoever may view the queue: the total of /queue/dashboard counts
 *   - for whoever decides claims: the claim ids in their /work/inbox
 * The list of alerts is kept per badge in this browser (30 at most). An alert only points at a page; opening it still goes through the
 * server's permission checks. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  var POLL_MS = 7000, KEEP = 30;
  var st = { badge: null, timer: null, seq: null, total: null, inbox: null, demo: false, items: [], open: false, audio: null };

  // ---- sound: three rising notes for the signature chime (the three nodes of the logo's check), and a few alternatives
  CG.SOUNDS = [['chime', 'Guard chime'], ['ping', 'Soft ping'], ['double', 'Double tap'], ['pulse', 'Synth pulse'], ['none', 'Silent']];
  var NOTES = {
    chime: [[659.25, 0, 0.22, 'sine'], [830.61, 0.12, 0.22, 'sine'], [987.77, 0.24, 0.5, 'sine'], [1975.5, 0.24, 0.35, 'triangle', 0.18]],
    ping: [[1318.5, 0, 0.45, 'sine'], [2637, 0, 0.2, 'sine', 0.12]],
    double: [[880, 0, 0.1, 'triangle'], [880, 0.16, 0.14, 'triangle']],
    pulse: [[392, 0, 0.16, 'sawtooth', 0.35], [587.33, 0.1, 0.18, 'square', 0.25], [783.99, 0.2, 0.38, 'sine']]
  };
  var VOLUME = { low: 0.05, medium: 0.12, high: 0.24 };
  function audio() {
    var Ctx = window.AudioContext || window.webkitAudioContext;
    if (!Ctx) return null;
    if (!st.audio) { try { st.audio = new Ctx(); } catch (e) { return null; } }
    if (st.audio.state === 'suspended') st.audio.resume().catch(function () {});
    return st.audio;
  }
  // browsers only start audio after a click or a key press: open the audio context on the first one
  function unlock() { audio(); document.removeEventListener('pointerdown', unlock); document.removeEventListener('keydown', unlock); }
  document.addEventListener('pointerdown', unlock); document.addEventListener('keydown', unlock);

  CG.playSound = function (name, volume) {
    var notes = NOTES[name], ctx = notes && audio();
    if (!ctx) return;
    var t0 = ctx.currentTime + 0.02, peak = VOLUME[volume] || VOLUME.medium;
    notes.forEach(function (n) {
      var osc = ctx.createOscillator(), gain = ctx.createGain(), at = t0 + n[1], len = n[2], g = peak * (n[4] || 1);
      osc.type = n[3]; osc.frequency.setValueAtTime(n[0], at);
      gain.gain.setValueAtTime(0.0001, at);
      gain.gain.exponentialRampToValueAtTime(g, at + 0.012);
      gain.gain.exponentialRampToValueAtTime(0.0001, at + len);
      osc.connect(gain); gain.connect(ctx.destination);
      osc.start(at); osc.stop(at + len + 0.05);
    });
  };

  // ---- the stored list: only known fields with checked values come back from storage
  function key() { return 'cg.notif.v1.' + st.badge; }
  function okHref(x) { return typeof x === 'string' && /^#\/[a-z]+(\/[A-Za-z0-9_-]{1,64})?$/.test(x); }
  function load() {
    var raw = [];
    try { raw = JSON.parse(localStorage.getItem(key()) || '[]'); } catch (e) { raw = []; }
    st.items = (Array.isArray(raw) ? raw : []).filter(function (i) {
      return i && typeof i.title === 'string' && typeof i.text === 'string' && typeof i.at === 'number' && (i.kind === 'arrival' || i.kind === 'inbox') && okHref(i.href);
    }).slice(0, KEEP).map(function (i) { return { id: String(i.id).slice(0, 40), kind: i.kind, title: i.title.slice(0, 80), text: i.text.slice(0, 200), href: i.href, at: i.at, read: !!i.read }; });
  }
  function persist() { try { localStorage.setItem(key(), JSON.stringify(st.items)); } catch (e) { /* private window: this page only */ } }
  function unread() { return st.items.filter(function (i) { return !i.read; }).length; }

  function add(item) {
    item.id = Date.now().toString(36) + Math.random().toString(36).slice(2, 6); item.at = Date.now(); item.read = false;
    st.items.unshift(item); st.items = st.items.slice(0, KEEP); persist(); paint();
    var p = CG.prefs.all();
    if (p.notifySound !== 'none') CG.playSound(p.notifySound, p.notifyVolume);
    CG.toast(item.title + ': ' + item.text, 'info');
    if (p.notifyDesktop && document.hidden && window.Notification && Notification.permission === 'granted') {
      try { var n = new Notification('ClaimGuard AI · ' + item.title, { body: item.text, icon: 'icon.svg', tag: 'cg-' + item.kind }); n.onclick = function () { window.focus(); location.hash = item.href; n.close(); }; } catch (e) { /* not supported here */ }
    }
  }

  // ---- polling
  function plural(n, one, many) { return n + ' ' + (n === 1 ? one : many); }
  function arrivals(n, ids) {
    var canView = CG.has('claims.view');
    add({ kind: 'arrival', title: plural(n, 'new claim', 'new claims') + ' arrived',
      text: (ids.length ? ids.slice(0, 3).join(', ') + (ids.length > 3 ? ' and ' + (ids.length - 3) + ' more' : '') + ' · ' : '') + 'checked by the engine and waiting in the queue.',
      href: canView && ids.length === 1 ? '#/claim/' + ids[0] : canView ? '#/claims' : CG.has('queue.view') ? '#/queue' : '#/home' });
  }
  function pollFeed() {
    return fetch('/demo/feed', { credentials: 'same-origin', headers: { Accept: 'application/json' } }).then(function (res) {
      if (!res.ok) throw new Error('no feed');
      return res.json();
    }).then(function (d) {
      st.demo = true;
      if (typeof d.seq !== 'number') return;
      if (st.seq !== null && d.seq > st.seq) {
        var fresh = (d.recent || []).filter(function (r) { return r.seq > st.seq && typeof r.claim_id === 'string'; }).map(function (r) { return r.claim_id; });
        arrivals(d.seq - st.seq, fresh);
      }
      st.seq = d.seq;
    }, function () {
      st.demo = false;
      if (!CG.has('queue.view')) return;
      return CG.api('GET', '/queue/dashboard').then(function (d) {
        var total = 0; Object.keys(d.counts || {}).forEach(function (k) { total += d.counts[k] || 0; });
        if (st.total !== null && total > st.total) arrivals(total - st.total, []);
        st.total = total;
      }, function () {});
    });
  }
  function pollInbox() {
    if (!CG.has('claims.decide') || !CG.prefs.get('notifyInbox')) return Promise.resolve();
    return CG.api('GET', '/work/inbox').then(function (d) {
      var ids = (d.claims || []).map(function (c) { return c.claim_id; }).filter(function (x) { return typeof x === 'string'; });
      if (st.inbox) {
        var fresh = ids.filter(function (x) { return st.inbox.indexOf(x) < 0; });
        if (fresh.length) add({ kind: 'inbox', title: plural(fresh.length, 'claim', 'claims') + ' routed to you', text: fresh.slice(0, 3).join(', ') + (fresh.length > 3 ? ' and ' + (fresh.length - 3) + ' more' : '') + ' · now in your inbox.',
          href: fresh.length === 1 ? '#/work/' + fresh[0] : '#/inbox' });
      }
      st.inbox = ids;
    }, function () {});
  }
  function tick() {
    if (!CG.state.me || CG.state.me.badge_id !== st.badge) { CG.notify.stop(); return; }
    if (!CG.prefs.get('notify')) return;
    pollFeed().then(pollInbox);
  }

  // ---- the bell and its panel
  function ago(t) { var s = Math.max(1, Math.round((Date.now() - t) / 1000)); return s < 60 ? s + ' s ago' : s < 3600 ? Math.round(s / 60) + ' min ago' : Math.round(s / 3600) + ' h ago'; }
  function paint() {
    var n = unread();
    Array.prototype.forEach.call(document.querySelectorAll('.bell'), function (b) {
      var c = b.querySelector('.bell-n'); c.textContent = n > 9 ? '9+' : String(n); c.hidden = !n;
      b.setAttribute('aria-label', n ? 'Notifications, ' + n + ' unread' : 'Notifications');
      b.classList.toggle('has', !!n);
    });
    paintTitle();
    if (st.open) drawPanel();
  }
  function paintTitle() { var t = document.title.replace(/^\(\d+\+?\) /, ''), n = st.badge ? unread() : 0; document.title = n ? '(' + (n > 9 ? '9+' : n) + ') ' + t : t; }
  var setTitle = CG.setTitle;
  CG.setTitle = function (t) { setTitle(t); paintTitle(); };

  function closePanel() {
    st.open = false; var p = document.getElementById('cg-notif'); if (p) p.remove();
    document.removeEventListener('pointerdown', outside, true); document.removeEventListener('keydown', onKey, true);
    Array.prototype.forEach.call(document.querySelectorAll('.bell'), function (b) { b.setAttribute('aria-expanded', 'false'); });
  }
  function outside(e) { var p = document.getElementById('cg-notif'); if (p && !p.contains(e.target) && !e.target.closest('.bell')) closePanel(); }
  function onKey(e) { if (e.key === 'Escape') { closePanel(); var b = document.querySelector('.bell'); if (b) b.focus(); } }
  function drawPanel() {
    var old = document.getElementById('cg-notif'), list = h('ul', { class: 'notif-list' });
    st.items.forEach(function (i) {
      list.appendChild(h('li', { class: i.read ? '' : 'unread' }, h('a', { href: i.href, onclick: function () { i.read = true; persist(); closePanel(); paint(); } },
        h('span', { class: 'notif-ic ' + i.kind }, CG.icon(i.kind === 'inbox' ? 'inbox' : 'bell')),
        h('span', { class: 'notif-tx' }, h('b', { text: i.title }), h('span', { text: i.text }), h('small', { text: ago(i.at) })))));
    });
    var panel = h('section', { class: 'notif-panel', id: 'cg-notif', role: 'dialog', 'aria-label': 'Notifications' },
      h('header', null, h('b', { text: 'Notifications' }),
        h('div', { class: 'row tight' },
          h('button', { class: 'btn ghost small', type: 'button', text: 'Mark all read', disabled: !unread(), onclick: function () { st.items.forEach(function (i) { i.read = true; }); persist(); paint(); } }),
          h('button', { class: 'btn ghost small', type: 'button', text: 'Clear', disabled: !st.items.length, onclick: function () { st.items = []; persist(); paint(); } }))),
      st.items.length ? list : h('p', { class: 'notif-empty', text: CG.prefs.get('notify') ? 'Nothing new. You will hear a sound when claims arrive.' : 'Notifications are off. Turn them on in Customize.' }),
      h('footer', null,
        st.demo ? h('button', { class: 'btn small primary', type: 'button', onclick: simulate }, CG.icon('bolt'), 'Simulate new claims (demo)') : null,
        h('a', { class: 'btn small ghost', href: '#/customize', onclick: function () { CG.state.customTab = 'alerts'; closePanel(); } }, CG.icon('sliders'), 'Settings')));
    if (old) old.replaceWith(panel); else document.body.appendChild(panel);
  }
  function simulate() {
    var n = 1 + Math.floor(Math.random() * 3);
    fetch('/demo/arrivals?n=' + n, { method: 'POST', credentials: 'same-origin', headers: { Accept: 'application/json', 'X-CG-Demo': '1' } }).then(function (res) {
      if (!res.ok) throw new Error();
      return res.json();
    }).then(function (d) {
      CG.toast((d.accepted || 0) ? 'Sent ' + plural(d.accepted, 'claim', 'claims') + ' to intake. The alert follows in a few seconds.' : 'No more sample claims to send.', d.accepted ? 'good' : 'warn');
      setTimeout(tick, 1200);
    }, function () { CG.toast('The demo could not add claims.', 'bad'); });
  }

  CG.notify = {
    bell: function () {
      var b = h('button', { class: 'btn small ghost bell', type: 'button', 'aria-haspopup': 'dialog', 'aria-expanded': String(st.open), title: 'Notifications' },
        CG.icon('bell'), h('span', { class: 'bell-n', hidden: true }));
      b.addEventListener('click', function (e) {
        e.stopPropagation();
        if (st.open) { closePanel(); return; }
        st.open = true; drawPanel(); b.setAttribute('aria-expanded', 'true');
        document.addEventListener('pointerdown', outside, true); document.addEventListener('keydown', onKey, true);
        var first = document.querySelector('#cg-notif a, #cg-notif button'); if (first) first.focus();
      });
      setTimeout(paint, 0);
      return b;
    },
    // called by every shell render: starts once per signed-in badge
    start: function () {
      var me = CG.state.me;
      if (!me || (st.badge === me.badge_id && st.timer)) return;
      CG.notify.stop();
      st.badge = me.badge_id; st.seq = st.total = st.inbox = null; load();
      st.timer = setInterval(tick, POLL_MS); tick(); paint();
    },
    stop: function () { if (st.timer) clearInterval(st.timer); st.timer = null; st.badge = null; st.items = []; closePanel(); paintTitle(); },
    test: function () { var p = CG.prefs.all(); CG.playSound(p.notifySound === 'none' ? 'chime' : p.notifySound, p.notifyVolume); },
    simulate: simulate,
    isDemo: function () { return st.demo; }
  };
})();
