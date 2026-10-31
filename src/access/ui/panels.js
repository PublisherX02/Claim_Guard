/* ClaimGuard console: movable panels. Each panel of the claim screen carries a grip; dragging the grip with the mouse (or a finger) puts the panel
 * above, below or in the other column, and the arrangement is kept in this browser for this badge. The same moves work from the keyboard
 * (grip focused: arrow keys), so the picture is never the only way to do it. Panels are matched by a fixed key, never by position, so one
 * saved arrangement applies to every claim. Nothing here touches claim data. */
(function () {
  'use strict';
  var CG = window.CG, h = CG.h;
  var COLS = ['main', 'side'];

  function storeKey() { return 'cg.panels.' + (CG.state.me ? CG.state.me.badge_id : 'anon'); }
  function readLayout() {
    try {
      var v = JSON.parse(localStorage.getItem(storeKey()) || 'null');
      if (!v || typeof v !== 'object') return null;
      var out = {};
      COLS.forEach(function (c) { out[c] = Array.isArray(v[c]) ? v[c].filter(function (k) { return typeof k === 'string' && k.length < 40; }).slice(0, 20) : []; });
      return out;
    } catch (e) { return null; }
  }
  function writeLayout(v) { try { if (v) localStorage.setItem(storeKey(), JSON.stringify(v)); else localStorage.removeItem(storeKey()); } catch (e) { /* private window: the arrangement lasts for this page only */ } }

  CG.panels = {
    /* Marks an element as a movable panel and gives it a grip. The element must later be a direct child of one of the two columns. */
    tag: function (el, key, title) {
      if (!el) return el;
      el.setAttribute('data-panel', key);
      el.classList.add('movable');
      el.appendChild(h('button', { class: 'grip', type: 'button', 'data-grip': key, title: 'Drag to move "' + title + '". With the keyboard: arrow keys.', 'aria-label': 'Move panel: ' + title + '. Arrow keys move it up, down or to the other column.' }, CG.icon('grip')));
      return el;
    },

    /* cols = { main: element, side: element }, root = the element holding both. Applies the saved arrangement and wires dragging. */
    enable: function (root, cols) {
      var layout = readLayout(), dragging = null;
      function panels(col) { return Array.prototype.filter.call(cols[col].children, function (c) { return c.getAttribute && c.getAttribute('data-panel'); }); }
      function all() { return panels('main').concat(panels('side')); }
      function current() { var o = {}; COLS.forEach(function (c) { o[c] = panels(c).map(function (p) { return p.getAttribute('data-panel'); }); }); return o; }
      function save() { layout = current(); writeLayout(layout); resetBtn.hidden = false; }

      all().forEach(function (p) { p.setAttribute('data-default-col', cols.main.contains(p) ? 'main' : 'side'); });   // before any saved move, so a new panel lands where it starts
      if (layout) {
        var byKey = {};
        all().forEach(function (p) { byKey[p.getAttribute('data-panel')] = p; });
        COLS.forEach(function (c) { layout[c].forEach(function (k) { if (byKey[k]) { cols[c].appendChild(byKey[k]); delete byKey[k]; } }); });
        // a panel the saved arrangement does not know keeps its own column; the saved ones were moved to the end of theirs above, so put new ones after
        Object.keys(byKey).forEach(function (k) { cols[byKey[k].getAttribute('data-default-col') || 'main'].appendChild(byKey[k]); });
      }

      var resetBtn = h('button', { class: 'btn small', type: 'button', text: 'Reset layout', hidden: !layout, onclick: function () {
        writeLayout(null); layout = null; CG.toast('Layout reset. The panels are back where they started.', 'good'); if (CG.rerender) CG.rerender(); } });
      root.parentNode.insertBefore(h('div', { class: 'layoutbar' }, h('span', { class: 'muted small', text: 'Drag the grip on any panel to put it where you want it; your layout is kept for your badge on this computer.' }), resetBtn), root);

      function place(el, col, before) { cols[col].insertBefore(el, before || null); }
      function colAt(x, y) {
        var best = 'main', bestD = Infinity;
        COLS.forEach(function (c) {
          var r = cols[c].getBoundingClientRect(), dx = x < r.left ? r.left - x : x > r.right ? x - r.right : 0, dy = y < r.top ? r.top - y : y > r.bottom ? y - r.bottom : 0, d = dx * dx + dy * dy;
          if (d < bestD) { bestD = d; best = c; }
        });
        return best;
      }
      function beforeAt(col, y) {
        var list = panels(col).filter(function (p) { return p !== dragging; });
        for (var i = 0; i < list.length; i++) { var r = list[i].getBoundingClientRect(); if (y < r.top + r.height / 2) return list[i]; }
        return null;
      }

      root.addEventListener('pointerdown', function (e) {
        var grip = e.target.closest ? e.target.closest('[data-grip]') : null;
        if (!grip || (e.button !== undefined && e.button !== 0)) return;
        dragging = grip.closest('[data-panel]');
        if (!dragging) return;
        e.preventDefault();
        try { grip.setPointerCapture(e.pointerId); } catch (x) { /* older engine: the move events still reach the grip while the button is down */ }
        dragging.classList.add('dragging'); root.classList.add('arranging');
      });
      root.addEventListener('pointermove', function (e) {
        if (!dragging) return;
        var col = colAt(e.clientX, e.clientY), before = beforeAt(col, e.clientY);
        if (dragging.parentNode !== cols[col] || dragging.nextSibling !== before) place(dragging, col, before);
        if (e.clientY < 70) window.scrollBy(0, -18); else if (e.clientY > window.innerHeight - 70) window.scrollBy(0, 18);   // reach panels off screen
      });
      function drop() { if (!dragging) return; dragging.classList.remove('dragging'); root.classList.remove('arranging'); dragging = null; save(); }
      root.addEventListener('pointerup', drop);
      root.addEventListener('pointercancel', drop);

      root.addEventListener('keydown', function (e) {
        var grip = e.target.closest ? e.target.closest('[data-grip]') : null;
        if (!grip) return;
        var p = grip.closest('[data-panel]'), col = cols.main.contains(p) ? 'main' : 'side', list = panels(col), i = list.indexOf(p);
        if (e.key === 'ArrowUp' && i > 0) place(p, col, list[i - 1]);
        else if (e.key === 'ArrowDown' && i < list.length - 1) place(p, col, list[i + 2] || null);
        else if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
          var to = e.key === 'ArrowLeft' ? 'main' : 'side';
          if (to === col) return;
          place(p, to, null);
        } else return;
        e.preventDefault(); grip.focus(); save();
      });
    }
  };
})();
