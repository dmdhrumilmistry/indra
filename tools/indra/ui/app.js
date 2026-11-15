// indra UI. Plain JavaScript, no build step, no third-party code.
// Every string from the server is rendered with textContent, never as HTML.
(function () {
  "use strict";

  // ---- tiny helpers ----------------------------------------------------------
  // A select set to a value it has no option for shows blank (browsers do
  // that silently): a profile or agent reporting a mode, model, runner or node
  // this page did not list. Everywhere: such a value gets its own option, and
  // a select left with nothing chosen falls back to its first option.
  (function () {
    var d = Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype, "value");
    if (!d || !d.set) return;
    Object.defineProperty(HTMLSelectElement.prototype, "value", {
      configurable: true, enumerable: d.enumerable,
      get: function () { return d.get.call(this); },
      set: function (v) {
        d.set.call(this, v);
        if (this.selectedIndex >= 0) return;
        var s = v === null || v === undefined ? "" : String(v);
        if (s !== "") { this.appendChild(new Option(s, s, true, true)); return; }
        if (this.options.length) this.selectedIndex = 0;
      }
    });
  })();

  var OPENED = {};
  function h(tag, props) {
    var el = document.createElement(tag);
    var kids = Array.prototype.slice.call(arguments, 2);
    if (props) {
      Object.keys(props).forEach(function (k) {
        var v = props[k];
        if (v === null || v === undefined || v === false) return;
        if (k === "class") el.className = v;
        else if (k === "text") el.textContent = v;
        else if (k.slice(0, 2) === "on") el.addEventListener(k.slice(2), v);
        else if (k === "value") { if (tag !== "select") el.value = v; }
        else if (k === "keep") {
          // A section the page redraws while it refreshes: it stays as the
          // user left it (open or closed) instead of snapping shut.
          if (OPENED[v] !== undefined) el.open = OPENED[v];
          el.addEventListener("toggle", function () { OPENED[v] = el.open; });
        }
        else if (k === "hidden") el.hidden = !!v;
        else el.setAttribute(k, v === true ? "" : v);
      });
    }
    kids.forEach(function add(c) {
      if (c === null || c === undefined || c === false) return;
      if (Array.isArray(c)) c.forEach(add);
      else el.appendChild(typeof c === "string" || typeof c === "number" ? document.createTextNode(String(c)) : c);
    });
    // A select's value once its options are there.
    if (tag === "select" && props && props.value !== undefined && props.value !== null) el.value = props.value;
    return el;
  }
  function $(id) { return document.getElementById(id); }
  function q(params) {
    var s = Object.keys(params).filter(function (k) { return params[k] !== undefined && params[k] !== null; })
      .map(function (k) { return encodeURIComponent(k) + "=" + encodeURIComponent(params[k]); }).join("&");
    return s ? "?" + s : "";
  }
  function ago(ts) {
    if (!ts) return "";
    var d = Math.max(0, Date.now() / 1000 - ts);
    if (d < 60) return Math.floor(d) + "s ago";
    if (d < 3600) return Math.floor(d / 60) + "m ago";
    if (d < 86400) return Math.floor(d / 3600) + "h ago";
    return Math.floor(d / 86400) + "d ago";
  }
  function clock(ts) {
    var d = new Date(ts * 1000);
    return d.toTimeString().slice(0, 8);
  }
  function bytes(n) { return n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KB" : n < 1073741824 ? (n / 1048576).toFixed(1) + " MB" : (n / 1073741824).toFixed(1) + " GB"; }
  function store(k, v) { try { if (v === undefined) return localStorage.getItem(k); localStorage.setItem(k, v); } catch (e) { return null; } }
  // A short note at the corner. An error stays until it is closed (or the
  // next note), so it is not missed; screen readers hear it at once.
  function toast(msg, bad) {
    Array.prototype.forEach.call(document.querySelectorAll(".toast"), function (x) { x.remove(); });
    var t = h("div", { class: "toast" + (bad ? " bad" : ""), role: bad ? "alert" : "status" }, h("span", { text: msg }),
      bad ? h("button", { type: "button", class: "x", "aria-label": "Close", onclick: function () { t.remove(); } }, "x") : null);
    document.body.appendChild(t);
    if (!bad) setTimeout(function () { t.remove(); }, 2600);
  }

  // A toast with one button (e.g. Undo), shown a little longer.
  function toastAction(msg, label, fn) {
    Array.prototype.forEach.call(document.querySelectorAll(".toast"), function (x) { x.remove(); });
    var t = h("div", { class: "toast act", role: "status" }, h("span", { text: msg }),
      h("button", { type: "button", onclick: function () { t.remove(); fn(); } }, label));
    document.body.appendChild(t);
    setTimeout(function () { t.remove(); }, 7000);
  }

  var S = { ov: null, h: store("indra.h") || "", cleanup: [] };

  // Save text as a file the browser downloads.
  function saveFile(name, text, type) {
    var url = URL.createObjectURL(new Blob([text], { type: type || "application/octet-stream" }));
    var a = h("a", { href: url, download: name, hidden: true });
    document.body.appendChild(a); a.click(); a.remove();
    setTimeout(function () { URL.revokeObjectURL(url); }, 4000);
  }
  function jsonFile(name, data) { saveFile(name, JSON.stringify(data, null, 2) + "\n", "application/json"); }
  // Ask for files, or a whole folder. Resolves with [{path, name, text}] for
  // the files `keep` accepts (by name and size), plus how many were skipped.
  function pickFiles(o) {
    return new Promise(function (resolve) {
      var inp = h("input", { type: "file", hidden: true, accept: o.accept || null });
      if (o.multiple || o.folder) inp.multiple = true;
      if (o.folder) inp.setAttribute("webkitdirectory", "");
      inp.addEventListener("change", function () {
        var all = Array.prototype.slice.call(inp.files || []); inp.remove();
        var keep = all.filter(function (f) { return f.size <= 5 * 1048576 && (!o.keep || o.keep(f.name)); });
        Promise.all(keep.map(function (f) { return f.text().then(function (t) { return { path: f.webkitRelativePath || f.name, name: f.name, text: t }; }); }))
          .then(function (files) {
            files.skipped = all.length - keep.length;
            files.all = all.map(function (f) { return f.webkitRelativePath || f.name; });
            resolve(files);
          });
      });
      document.body.appendChild(inp); inp.click();
    });
  }
  // A JSON file holding a list: [ ... ] or { items: [ ... ] }.
  function listFrom(text) {
    var v = JSON.parse(text);
    if (Array.isArray(v)) return v;
    if (v && Array.isArray(v.items)) return v.items;
    throw new Error("expected a JSON list");
  }

  function api(path, opts) {
    opts = opts || {};
    var init = { method: opts.method || "GET", credentials: "same-origin", headers: {} };
    if (opts.body !== undefined) { init.headers["content-type"] = "application/json"; init.body = JSON.stringify(opts.body); }
    return fetch(path, init).then(function (r) {
      return r.text().then(function (t) {
        var data = null;
        try { data = t ? JSON.parse(t) : null; } catch (e) { data = { error: t }; }
        if (r.status === 401) { authWall(); throw new Error("unauthorized"); }
        if (!r.ok) { var err = new Error((data && data.error) || r.statusText); err.status = r.status; throw err; }
        if (init.method !== "GET") S.dirty = false;
        return data;
      });
    });
  }
  // A link from outside data (a marketplace, a tracker): web addresses only.
  function safeUrl(u) { return typeof u === "string" && /^https?:\/\//i.test(u) ? u : null; }
  function hq(extra) { var p = { h: S.h }; Object.keys(extra || {}).forEach(function (k) { p[k] = extra[k]; }); return q(p); }
  function harness() { return S.ov && (S.ov.harnesses.find(function (x) { return x.name === S.h; }) || S.ov.harnesses[0]); }

  // Not signed in: a sign-in form when the console has users, else how to
  // use the access token.
  var walled = false;
  function authWall() {
    if (walled) return;
    walled = true;
    document.body.classList.add("walled");
    var m = $("main");
    m.textContent = "";
    fetch("/api/session", { credentials: "same-origin" }).then(function (r) { return r.json(); }).catch(function () { return {}; }).then(function (sess) {
      if (!sess.users) {
        m.appendChild(h("div", { class: "authwall card" },
          h("h1", { text: "Access token needed" }),
          h("p", { text: "indra printed a URL containing an access token when it started. Open that URL once in this browser; it sets a cookie and you will not need it again until indra restarts." }),
          h("p", { class: "muted", text: "For named accounts with roles instead, an admin adds users in Settings > Users (or `indra user add NAME`)." })));
        return;
      }
      var name = h("input", { type: "text", autocomplete: "username", required: true, "aria-label": "User name" });
      var pw = h("input", { type: "password", autocomplete: "current-password", required: true, "aria-label": "Password" });
      var err = h("div", { class: "note bad", hidden: true, role: "alert" });
      var go = h("button", { class: "primary", type: "submit" }, "Sign in");
      m.appendChild(h("form", { class: "authwall card form signin", onsubmit: function (e) {
        e.preventDefault(); go.disabled = true; err.hidden = true;
        fetch("/api/login", { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json" }, body: JSON.stringify({ name: name.value.trim(), password: pw.value }) })
          .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); })
          .then(function (r) {
            if (!r.ok) { err.textContent = r.j.error || "Sign-in failed"; err.hidden = false; go.disabled = false; pw.select(); return; }
            location.reload();
          }).catch(function (e2) { err.textContent = e2.message; err.hidden = false; go.disabled = false; });
      } },
        h("h1", { text: "Sign in to indra" }),
        h("label", null, "User name", name), h("label", null, "Password", pw), err,
        h("div", { class: "row" }, go),
        h("p", { class: "muted small", text: "No account? Ask an admin of this console. Admins manage users in Settings > Users or with `indra user`." })));
      name.focus();
    });
  }

  function signOut() {
    fetch("/api/logout", { method: "POST", credentials: "same-origin", headers: { "content-type": "application/json" }, body: "{}" }).then(function () { location.hash = ""; location.reload(); });
  }

  // A small dialog with fields (passwords masked). `submit(values)` returns a
  // promise; its error shows in the dialog, which stays open to try again.
  function ask(title, fields, label, submit) {
    var dlg = h("dialog", { class: "ask", "aria-label": title });
    var inputs = fields.map(function (f) {
      var el = h("input", { type: f.type || "text", required: f.optional ? null : true, minlength: f.min ? String(f.min) : null,
        autocomplete: f.autocomplete || (f.type === "password" ? "new-password" : "off"), placeholder: f.placeholder || "" });
      if (f.value) el.value = f.value;
      return { f: f, el: el };
    });
    var err = h("div", { class: "note bad", role: "alert", hidden: true });
    var go = h("button", { type: "submit", class: "primary" }, label);
    var form = h("form", { class: "form", onsubmit: function (e) {
      e.preventDefault();
      var v = {}; inputs.forEach(function (x) { v[x.f.name] = x.el.value; });
      go.disabled = true; err.hidden = true;
      Promise.resolve().then(function () { return submit(v); }).then(function () { dlg.close(); dlg.remove(); })
        .catch(function (x) { err.textContent = x.message; err.hidden = false; go.disabled = false; });
    } }, h("h3", { text: title }), inputs.map(function (x) { return h("label", null, x.f.label, x.f.hint ? h("small", { text: x.f.hint }) : null, x.el); }), err,
      h("div", { class: "row" }, go, h("button", { type: "button", onclick: function () { dlg.close(); dlg.remove(); } }, "Cancel")));
    dlg.appendChild(form);
    dlg.addEventListener("cancel", function () { dlg.remove(); });
    document.body.appendChild(dlg);
    dlg.showModal();
    if (inputs[0]) inputs[0].el.focus();
  }

  function changeOwnPassword() {
    ask("Change your password", [
      { name: "cur", label: "Current password", type: "password", autocomplete: "current-password" },
      { name: "next", label: "New password", type: "password", min: 10, hint: "At least 10 characters." },
      { name: "again", label: "New password again", type: "password", min: 10 }
    ], "Change password", function (v) {
      if (v.next !== v.again) throw new Error("The two new passwords differ.");
      return api("/api/session/password", { method: "PUT", body: { current: v.cur, new: v.next } }).then(function () { toast("Password changed; sign in again"); setTimeout(signOut, 800); });
    });
  }

  // ---- force-directed graph on a canvas ---------------------------------------
  var PALETTE = ["#2f6fdf", "#d9622b", "#23875c", "#b8459c", "#c79a1e", "#5b6ad0", "#c2434a", "#3a9fb5", "#7a8b2a", "#8a5a3c"];
  function cssVar(n) { return getComputedStyle(document.documentElement).getPropertyValue(n).trim(); }

  function ForceGraph(box, data, onSelect) {
    var canvas = h("canvas"), ctx = canvas.getContext("2d");
    box.appendChild(canvas);
    var nodes = data.nodes.map(function (n, i) {
      var a = i * 2.399963, r = 14 * Math.sqrt(i + 1);
      return { id: n.id, title: n.title, type: n.type || (n.ghost ? "(missing)" : "note"), ghost: n.ghost, raw: n,
        x: Math.cos(a) * r, y: Math.sin(a) * r, vx: 0, vy: 0, deg: 0, out: [], inc: [] };
    });
    var byId = {}; nodes.forEach(function (n) { byId[n.id] = n; });
    var edges = [];
    data.edges.forEach(function (e) {
      var s = byId[e.source], t = byId[e.target];
      if (!s || !t) return;
      edges.push({ s: s, t: t, rel: e.rel }); s.out.push({ n: t, rel: e.rel }); t.inc.push({ n: s, rel: e.rel }); s.deg++; t.deg++;
    });
    nodes.forEach(function (n) { n.r = 5 + Math.min(10, Math.sqrt(n.deg) * 2.2); });
    var types = {}; nodes.forEach(function (n) { types[n.type] = (types[n.type] || 0) + 1; });
    // Known kinds keep one colour everywhere; anything else takes the next free one.
    var FIXED = { agent: "#2f6fdf", skill: "#23875c", prompt: "#c79a1e", stage: "#b8459c" };
    var color = {}, hidden = {}, free = PALETTE.filter(function (c) { return Object.keys(FIXED).every(function (k) { return FIXED[k] !== c; }); });
    Object.keys(types).sort(function (a, b) { return types[b] - types[a]; }).forEach(function (t) {
      color[t] = t === "(missing)" ? null : FIXED[t] || free.shift() || PALETTE[0];
    });
    var legend = h("div", { class: "legend" });
    Object.keys(color).forEach(function (t) {
      var dot = h("i"); dot.style.background = color[t] || cssVar("--muted");
      var b = h("button", { class: "pill", type: "button", "aria-pressed": "true" }, dot, t + " " + types[t]);
      b.addEventListener("click", function () { hidden[t] = !hidden[t]; b.setAttribute("aria-pressed", String(!hidden[t])); heat(0.3); });
      legend.appendChild(b);
    });
    box.appendChild(legend);

    var W = 0, H = 0, dpr = 1, view = { x: 0, y: 0, k: 1 }, sel = null, hover = null, alpha = 1, running = false, drag = null, pan = null, moved = false;
    function vis(n) { return !hidden[n.type]; }
    function size() {
      var r = box.getBoundingClientRect(); dpr = window.devicePixelRatio || 1; W = r.width; H = r.height;
      canvas.width = W * dpr; canvas.height = H * dpr; draw();
    }
    function world(px, py) { var r = canvas.getBoundingClientRect(); return { x: (px - r.left - W / 2 - view.x) / view.k, y: (py - r.top - H / 2 - view.y) / view.k }; }
    function draw() {
      var C = { edge: cssVar("--edge"), hi: cssVar("--edge-hi"), fg: cssVar("--fg"), muted: cssVar("--muted") };
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0); ctx.clearRect(0, 0, W, H);
      ctx.translate(W / 2 + view.x, H / 2 + view.y); ctx.scale(view.k, view.k);
      var f = sel || hover, near = {};
      if (f) { near[f.id] = 1; f.out.concat(f.inc).forEach(function (l) { near[l.n.id] = 1; }); }
      ctx.lineWidth = 1 / view.k;
      edges.forEach(function (e) {
        if (!vis(e.s) || !vis(e.t)) return;
        var on = f && (e.s === f || e.t === f);
        ctx.strokeStyle = on ? C.hi : C.edge;
        ctx.beginPath(); ctx.moveTo(e.s.x, e.s.y); ctx.lineTo(e.t.x, e.t.y); ctx.stroke();
        if (on || view.k > 1.4) {
          var dx = e.t.x - e.s.x, dy = e.t.y - e.s.y, d = Math.sqrt(dx * dx + dy * dy) || 1, ux = dx / d, uy = dy / d;
          var tx = e.t.x - ux * (e.t.r + 2), ty = e.t.y - uy * (e.t.r + 2), s = 6 / view.k;
          ctx.fillStyle = on ? C.hi : C.edge; ctx.beginPath(); ctx.moveTo(tx, ty);
          ctx.lineTo(tx - ux * s - uy * s * .6, ty - uy * s + ux * s * .6); ctx.lineTo(tx - ux * s + uy * s * .6, ty - uy * s - ux * s * .6); ctx.fill();
        }
        if (on) {
          ctx.fillStyle = C.muted; ctx.font = (10 / view.k) + "px system-ui, sans-serif"; ctx.textAlign = "center";
          ctx.fillText(e.rel, (e.s.x + e.t.x) / 2, (e.s.y + e.t.y) / 2 - 3 / view.k);
        }
      });
      nodes.forEach(function (n) {
        if (!vis(n)) return;
        ctx.globalAlpha = f && !near[n.id] ? .18 : 1;
        ctx.beginPath(); ctx.arc(n.x, n.y, n.r, 0, Math.PI * 2);
        if (n.ghost || !color[n.type]) { ctx.strokeStyle = C.muted; ctx.setLineDash([3 / view.k, 3 / view.k]); ctx.stroke(); ctx.setLineDash([]); }
        else { ctx.fillStyle = color[n.type]; ctx.fill(); }
        if (n === sel) { ctx.strokeStyle = C.fg; ctx.lineWidth = 2.5 / view.k; ctx.stroke(); ctx.lineWidth = 1 / view.k; }
        if (view.k > 1.2 || near[n.id] || n.deg >= 3) {
          ctx.fillStyle = n.ghost ? C.muted : C.fg; ctx.textAlign = "center";
          ctx.font = (n === f ? "600 " : "") + (12 / view.k) + "px system-ui, sans-serif";
          ctx.fillText(n.title.length > 40 ? n.title.slice(0, 38) + "..." : n.title, n.x, n.y + n.r + 13 / view.k);
        }
        ctx.globalAlpha = 1;
      });
    }
    function tick() {
      var v = nodes.filter(vis), n = v.length, i, j;
      for (i = 0; i < n; i++) for (j = i + 1; j < n; j++) {
        var a = v[i], b = v[j], dx = b.x - a.x, dy = b.y - a.y, d2 = dx * dx + dy * dy + .01;
        if (d2 > 360000) continue;
        var f = 4000 * alpha / d2, d = Math.sqrt(d2); dx = dx / d * f; dy = dy / d * f;
        a.vx -= dx; a.vy -= dy; b.vx += dx; b.vy += dy;
      }
      edges.forEach(function (e) {
        if (!vis(e.s) || !vis(e.t)) return;
        var dx = e.t.x - e.s.x, dy = e.t.y - e.s.y, d = Math.sqrt(dx * dx + dy * dy) || 1, f = (d - 110) * .05 * alpha;
        e.s.vx += dx / d * f; e.s.vy += dy / d * f; e.t.vx -= dx / d * f; e.t.vy -= dy / d * f;
      });
      v.forEach(function (p) {
        p.vx -= p.x * .003 * alpha; p.vy -= p.y * .003 * alpha;
        if (p === drag) { p.vx = p.vy = 0; return; }
        p.vx *= .6; p.vy *= .6;
        p.x += Math.max(-40, Math.min(40, p.vx)); p.y += Math.max(-40, Math.min(40, p.vy));
      });
      alpha *= .985;
    }
    function loop() { if (dead) return; tick(); draw(); if (alpha > .01 || drag) requestAnimationFrame(loop); else running = false; }
    function heat(a) { alpha = Math.max(alpha, a); if (!running) { running = true; requestAnimationFrame(loop); } }
    function fit() {
      var v = nodes.filter(vis); if (!v.length) return;
      var x0 = Infinity, y0 = Infinity, x1 = -Infinity, y1 = -Infinity;
      v.forEach(function (n) { x0 = Math.min(x0, n.x); y0 = Math.min(y0, n.y); x1 = Math.max(x1, n.x); y1 = Math.max(y1, n.y); });
      var k = Math.min((W - 120) / Math.max(1, x1 - x0), (H - 120) / Math.max(1, y1 - y0));
      view.k = Math.max(.15, Math.min(1.6, k)); view.x = -(x0 + x1) / 2 * view.k; view.y = -(y0 + y1) / 2 * view.k; draw();
    }
    function pick(ev) {
      var w = world(ev.clientX, ev.clientY), best = null, bd = Infinity;
      nodes.forEach(function (n) {
        if (!vis(n)) return;
        var dx = n.x - w.x, dy = n.y - w.y, d = dx * dx + dy * dy, r = n.r + 6 / view.k;
        if (d < r * r && d < bd) { bd = d; best = n; }
      });
      return best;
    }
    canvas.addEventListener("pointerdown", function (ev) {
      canvas.setPointerCapture(ev.pointerId); moved = false;
      var n = pick(ev); if (n) { drag = n; heat(.2); } else pan = { x: ev.clientX - view.x, y: ev.clientY - view.y };
    });
    canvas.addEventListener("pointermove", function (ev) {
      if (drag) { var w = world(ev.clientX, ev.clientY); drag.x = w.x; drag.y = w.y; moved = true; return; }
      if (pan) { view.x = ev.clientX - pan.x; view.y = ev.clientY - pan.y; moved = true; draw(); return; }
      var n = pick(ev); if (n !== hover) { hover = n; canvas.style.cursor = n ? "pointer" : "grab"; draw(); }
    });
    canvas.addEventListener("pointerup", function (ev) {
      var was = drag; drag = null; pan = null;
      if (!moved) { sel = pick(ev); draw(); onSelect(sel); } else if (was) heat(.05);
    });
    canvas.addEventListener("wheel", function (ev) {
      ev.preventDefault();
      var r = canvas.getBoundingClientRect(), k = Math.min(6, Math.max(.1, view.k * Math.exp(-ev.deltaY * .0015)));
      var w = world(ev.clientX, ev.clientY);
      view.x = ev.clientX - r.left - W / 2 - w.x * k; view.y = ev.clientY - r.top - H / 2 - w.y * k; view.k = k; draw();
    }, { passive: false });
    var dead = false, ro = new ResizeObserver(size);
    ro.observe(box);
    size();
    for (var s = 0; s < 300 && nodes.length < 1500; s++) tick();
    fit(); heat(.05);
    return {
      select: function (id) { sel = byId[id] || null; if (sel) { view.x = -sel.x * view.k; view.y = -sel.y * view.k; } draw(); onSelect(sel); },
      redraw: draw, fit: fit,
      destroy: function () { dead = true; ro.disconnect(); }
    };
  }

  function graphView(container, data, panelFor) {
    var box = h("div", { class: "graphbox" });
    var panel = h("div", { class: "gpanel", hidden: true });
    container.appendChild(box);
    if (!data.nodes.length) { box.appendChild(h("div", { class: "empty", text: "Nothing to draw yet." })); return; }
    var g = ForceGraph(box, data, function (n) {
      panel.textContent = "";
      panel.hidden = !n;
      if (!n) return;
      panel.appendChild(h("button", { class: "ghost", type: "button", "aria-label": "Close", onclick: function () { g.select(null); } }, "x"));
      panelFor(n, panel, g);
    });
    box.appendChild(panel);
    S.cleanup.push(g.destroy);
    var mq = window.matchMedia("(prefers-color-scheme: dark)");
    var redraw = function () { g.redraw(); };
    mq.addEventListener("change", redraw);
    window.addEventListener("indra-theme", redraw);
    S.cleanup.push(function () { mq.removeEventListener("change", redraw); window.removeEventListener("indra-theme", redraw); });
    return g;
  }
  function linkList(title, items, onClick) {
    return [h("h3", { text: title }), items.length ? h("ul", { class: "links" }, items.map(function (l) {
      return h("li", null, h("span", { class: "rel", text: l.rel }), h("a", { href: "#", onclick: function (e) { e.preventDefault(); onClick(l); } }, l.title));
    })) : h("p", { class: "muted", text: "none" })];
  }

  // ---- pages -------------------------------------------------------------------
  function header(title, sub, actions) {
    return h("div", { class: "head" }, h("div", null, h("h1", { text: title }), sub ? h("p", { text: sub }) : null), actions ? h("div", { class: "row" }, actions) : null);
  }
  function empty(msg) { return h("div", { class: "empty", text: msg }); }
  function every(ms, fn) { var t = setInterval(fn, ms); S.cleanup.push(function () { clearInterval(t); }); }

  function stagePipe(stages) {
    var out = [];
    stages.forEach(function (s, i) {
      if (i) out.push(h("span", { class: "arrow", text: "->" }));
      out.push(h("span", { class: "stage " + (s.state || "") }, s.name, s.agent ? h("small", { text: s.agent }) : null));
    });
    return h("div", { class: "pipe" }, out);
  }

  function pageOverview(m) {
    var ov = S.ov;
    m.appendChild(header("Home", "indra runs AI coding agents (Claude Code, Codex, Gemini) on your code, on this computer or others you add, and shows what they do and find."));
    m.appendChild(getStarted(ov));
    var running = ov.procs.filter(function (p) { return p.status === "running"; });
    m.appendChild(h("h2", { text: "Running now" }));
    m.appendChild(running.length ? procTable(running) : empty("Nothing running. Start one with Run an agent."));
    var about = h("details", { class: "card", keep: "home-about" }, h("summary", { text: "About this setup: agents, skills and what they are made of" }),
      h("p", { class: "muted small", text: "A setup (harness) is a set of agents, skills and prompts for one kind of work. Pick one under Setup; More shows its parts." }));
    m.appendChild(about);
    m = about;
    var grid = h("div", { class: "grid" });
    ov.harnesses.forEach(function (x) {
      var c = x.counts;
      var card = h("div", { class: "card click" + (x.name === S.h ? " sel" : ""), tabindex: "0", role: "button",
        onclick: function () { setHarness(x.name); }, onkeydown: function (e) { if (e.key === "Enter") setHarness(x.name); } },
        h("div", { class: "row" }, h("h4", { text: x.name }), x.builtin ? h("span", { class: "pill", text: "built-in" }) : null,
          c.active_runs ? h("span", { class: "pill run live", text: c.active_runs + " live" }) : null),
        h("p", { class: "muted", text: x.description || x.root }),
        h("div", { class: "stats" },
          [["agents", c.agents], ["skills", c.skills], ["prompts", c.prompts], ["runs", c.runs], ["memory", c.memory]].map(function (s) {
            return h("div", { class: "stat" }, h("b", { text: String(s[1]) }), h("span", { text: s[0] }));
          })));
      grid.appendChild(card);
    });
    if (!ov.harnesses.length) grid.appendChild(empty("No harness found. Run indra inside a project, pass --target, or create a config with `indra init`."));
    m.appendChild(grid);

    var hs = harness();
    if (hs && hs.stages.length) {
      m.appendChild(h("h2", { text: "Pipeline: " + hs.name }));
      m.appendChild(stagePipe(hs.stages));
    }
    m.appendChild(h("h2", { text: "Agents installed here" }));
    m.appendChild(h("div", { class: "chips" }, ov.runners.map(function (r) {
      return h("span", { class: "pill " + (r.available ? "ok" : ""), title: r.program || (r.command + " is not on PATH"), text: r.name + (r.available ? "" : " (not installed)") });
    })));

  }

  // A short checklist for new users; it ticks itself off and can be hidden.
  function getStarted(ov) {
    if (store("indra.started") === "hidden") return h("div");
    var avail = ov.runners.filter(function (r) { return r.available; });
    var steps = [
      { done: avail.length > 0, title: "Have an agent installed", text: avail.length ? "Found: " + avail.map(function (r) { return r.name; }).join(", ") + "." : "Install Claude Code (or Codex, Gemini) on this computer, then reload this page.", href: null },
      { done: ov.procs.length > 0, title: "Run your first agent", text: "Pick a folder, type what it should do, press Run. You watch it work and answer its questions.", href: "#/launch", go: "Run an agent" },
      { done: ov.nodes.length > 1, title: "Add another machine (optional)", text: "One command on that machine, no SSH: agents can then run there too.", href: "#/nodes", go: "Add a machine" },
      { done: store("indra.seen.workflows") === "1", title: "Try a workflow (optional)", text: "Steps in order, like review then fix then test. Start from a built-in one or describe it and let Claude build it.", href: "#/workflows", go: "See workflows" }
    ];
    var left = steps.filter(function (x) { return !x.done; }).length;
    var box = h("div", { class: "card" },
      h("div", { class: "row" }, h("h3", { text: left ? "Get started" : "You are all set" }),
        h("button", { type: "button", class: "ghost", onclick: function () { store("indra.started", "hidden"); box.remove(); } }, "Hide")),
      h("ol", { class: "steps" }, steps.map(function (x, i) {
        return h("li", { class: x.done ? "done" : "" }, h("i", { text: x.done ? "ok" : String(i + 1) }),
          h("div", null, h("b", { text: x.title }), h("div", { class: "muted small", text: x.text }),
            !x.done && x.href ? h("a", { href: x.href, text: x.go + " ->" }) : null));
      })));
    return box;
  }

  function pageDefs(m, kind) {
    var titles = { agent: "Agents", skill: "Skills", prompt: "Prompts" };
    var subs = { agent: "Agent definitions: who does what, on which model, with which tools.",
      skill: "Skills the agents load. Click one to read or edit it.", prompt: "Reference documents and prompt files the harness uses." };
    var ro = S.ov.read_only || !S.ov.is_admin;
    m.appendChild(header(titles[kind], subs[kind], ro ? null : [
      h("a", { class: "btn", href: "#/market", text: "From a marketplace" }),
      h("button", { type: "button", onclick: function () { importDefs(); } }, "Import .md"),
      h("button", { type: "button", class: "primary", onclick: function () { newDef(); } }, "New " + kind)]));
    var formBox = h("div"); m.appendChild(formBox);
    function template(name) {
      return kind === "agent" ? "---\nname: " + name + "\ndescription: What it does, and when to use it\ntools: Read, Grep, Glob\n---\n\nYou are " + name + ".\n"
        : kind === "skill" ? "---\nname: " + name + "\ndescription: When an agent should load this skill\n---\n\n# " + name + "\n\n"
        : "# " + name + "\n\n";
    }
    function create(name, content) {
      return api("/api/def", { method: "POST", body: { h: S.h, kind: kind, name: name, content: content } });
    }
    function newDef() {
      var name = h("input", { type: "text", required: true, placeholder: "my-" + kind, pattern: "[A-Za-z0-9_.\\-]+" });
      var ta = h("textarea", { class: "code", spellcheck: "false", "aria-label": "Content" }); ta.value = template("my-" + kind);
      var touched = false; ta.addEventListener("input", function () { touched = true; });
      name.addEventListener("input", function () { if (!touched) ta.value = template(name.value.trim() || "my-" + kind); });
      formBox.textContent = "";
      formBox.appendChild(h("form", { class: "form card", onsubmit: function (e) {
        e.preventDefault();
        create(name.value.trim(), ta.value).then(function (r) { toast("Created " + r.path); if (r.id) location.hash = "#/def/" + encodeURIComponent(r.id); else route(); })
          .catch(function (err) { toast(err.message, true); });
      } }, h("h3", { text: "New " + kind }), h("label", null, "Name", h("small", { text: "Letters, digits, - and _. It becomes the file (or folder) name." }), name),
        h("label", null, "Content", ta), h("div", { class: "row" }, h("button", { type: "submit", class: "primary" }, "Create"), h("button", { type: "button", onclick: function () { formBox.textContent = ""; } }, "Cancel"))));
      name.focus();
    }
    function importDefs() {
      pickFiles({ multiple: true, accept: ".md,.markdown,text/markdown", keep: function (n) { return /\.(md|markdown)$/i.test(n); } }).then(function (fs) {
        if (!fs.length) return;
        var done = 0, failed = [];
        fs.reduce(function (p, f) {
          return p.then(function () {
            var parts = f.path.split("/"), stem = f.name.replace(/\.(md|markdown)$/i, "");
            var name = /^skill$/i.test(stem) && parts.length > 1 ? parts[parts.length - 2] : stem;
            return create(name, f.text).then(function () { done++; }).catch(function (e) { failed.push(f.name + ": " + e.message); });
          });
        }, Promise.resolve()).then(function () {
          toast("Imported " + done + " file(s)" + (failed.length ? "; " + failed.length + " failed: " + failed[0] : ""), failed.length > 0);
          route();
        });
      });
    }
    var filter = h("input", { type: "search", placeholder: "Filter by name or description", "aria-label": "Filter" });
    m.appendChild(h("div", { class: "row" }, filter));
    var holder = h("div");
    m.appendChild(h("div", { class: "tablewrap" }, holder));
    api("/api/defs" + hq()).then(function (defs) {
      defs = defs.filter(function (d) { return d.kind === kind; });
      function render() {
        var f = filter.value.trim().toLowerCase();
        var rows = defs.filter(function (d) { return !f || (d.name + " " + d.description).toLowerCase().indexOf(f) >= 0; });
        holder.textContent = "";
        if (!rows.length) { holder.appendChild(empty(defs.length ? "No match." : "None found for this harness.")); return; }
        holder.appendChild(h("table", null,
          h("thead", null, h("tr", null, h("th", { text: "Name" }), kind === "agent" ? h("th", { text: "Model" }) : null,
            kind !== "prompt" ? h("th", { text: "Tools" }) : null, h("th", { text: "Description" }), h("th", { text: "Updated" }))),
          h("tbody", null, rows.map(function (d) {
            return h("tr", { class: "click", tabindex: "0", onclick: function () { location.hash = "#/def/" + encodeURIComponent(d.id); },
              onkeydown: function (e) { if (e.key === "Enter") location.hash = "#/def/" + encodeURIComponent(d.id); } },
              h("td", null, h("b", { text: d.name }), h("div", { class: "muted", text: d.path })),
              kind === "agent" ? h("td", null, d.model ? h("span", { class: "pill", text: d.model }) : h("span", { class: "muted", text: "inherit" })) : null,
              kind !== "prompt" ? h("td", null, h("div", { class: "chips" }, d.tools.slice(0, 8).map(function (t) { return h("span", { class: "pill", text: t }); }))) : null,
              h("td", { class: "desc", text: d.description }),
              h("td", { class: "muted", text: ago(d.modified) }));
          }))));
      }
      filter.addEventListener("input", render);
      render();
    });
  }

  function pageDef(m, id) {
    api("/api/def" + hq({ id: id })).then(function (r) {
      var d = r.def, hash = r.hash, original = r.content;
      m.appendChild(header(d.name, d.description, [
        h("span", { class: "pill", text: d.kind }), d.model ? h("span", { class: "pill", text: d.model }) : null,
        h("button", { type: "button", onclick: function () { saveFile(d.abs.split(/[\\/]/).pop(), ta.value, "text/markdown"); } }, "Download"),
        (S.ov.read_only || !S.ov.is_admin) ? null : h("button", { type: "button", class: "danger", onclick: function () {
          if (!window.confirm("Delete " + d.path + "? This removes the file.")) return;
          api("/api/def" + hq({ id: d.id }), { method: "DELETE" }).then(function () {
            toast("Deleted " + d.path); location.hash = "#/" + ({ agent: "agents", skill: "skills", prompt: "prompts" }[d.kind] || "overview");
          }).catch(function (e) { toast(e.message, true); });
        } }, "Delete")]));
      var ta = h("textarea", { class: "code", spellcheck: "false", "aria-label": "File content", value: original });
      var save = h("button", { class: "primary", type: "button", disabled: true }, "Save");
      var revert = h("button", { type: "button", disabled: true }, "Revert");
      var state = h("span", { class: "muted", text: S.ov.read_only ? "Read-only: editing is disabled." : !S.ov.is_admin ? "Only admins can edit agents, skills and prompts." : "" });
      if ((S.ov.read_only || !S.ov.is_admin)) ta.readOnly = true;
      function dirty() { var x = ta.value !== original; save.disabled = !x || (S.ov.read_only || !S.ov.is_admin); revert.disabled = !x; state.textContent = x ? "Unsaved changes" : ""; }
      ta.addEventListener("input", dirty);
      ta.addEventListener("keydown", function (e) { if ((e.ctrlKey || e.metaKey) && e.key === "s") { e.preventDefault(); if (!save.disabled) save.click(); } });
      revert.addEventListener("click", function () { ta.value = original; dirty(); });
      save.addEventListener("click", function () {
        save.disabled = true;
        api("/api/def", { method: "PUT", body: { h: S.h, id: d.id, content: ta.value, hash: hash } }).then(function (res) {
          hash = res.hash; original = ta.value; dirty(); toast("Saved " + res.saved);
        }).catch(function (e) { toast(e.message, true); dirty(); });
      });
      window.onbeforeunload = function () { return ta.value !== original ? "" : undefined; };
      S.cleanup.push(function () { window.onbeforeunload = null; });
      var side = h("div", null,
        h("div", { class: "card" },
          h("h3", { text: "File" }), h("div", { class: "cmd", text: r.abs }),
          d.tools.length ? [h("h3", { text: "Tools" }), h("div", { class: "chips" }, d.tools.map(function (t) { return h("span", { class: "pill", text: t }); }))] : null,
          linkList("Uses", r.uses.map(function (u) { return { rel: u.rel, title: u.name, id: u.id }; }), function (l) { if (l.id.indexOf("stage:") !== 0) location.hash = "#/def/" + encodeURIComponent(l.id); }),
          linkList("Used by", r.used_by.map(function (u) { return { rel: u.rel, title: u.name, id: u.id }; }), function (l) { if (l.id.indexOf("stage:") !== 0) location.hash = "#/def/" + encodeURIComponent(l.id); }),
          h("h3", { text: "See it in context" }), h("a", { href: "#/graph/" + encodeURIComponent(d.id), text: "Open in the graph map" })));
      m.appendChild(h("div", { class: "split" },
        h("div", null, ta, h("div", { class: "row" }, save, revert, state, h("span", { class: "muted", text: "Ctrl/Cmd+S saves" }))), side));
    }).catch(function (e) { m.appendChild(empty(e.message)); });
  }

  var KIND_ROUTE = { agent: "agents", skill: "skills", prompt: "prompts" };

  function pageGraph(m, focus) {
    m.appendChild(header("Graph map", "How the harness fits together: stages run agents, agents spawn agents and invoke skills, and everything reads its prompts. Click a node for details."));
    api("/api/graph" + hq()).then(function (g) {
      var view = graphView(m, g, function (n, panel) {
        var raw = n.raw;
        panel.appendChild(h("h3", { text: n.type }));
        panel.appendChild(h("h2", { text: n.title }));
        if (raw.tags && raw.tags.length) panel.appendChild(h("p", null, h("span", { class: "pill", text: raw.tags[0] })));
        if (raw.body) panel.appendChild(h("p", { text: raw.body }));
        if (n.id.indexOf("stage:") !== 0) panel.appendChild(h("p", null, h("a", { href: "#/def/" + encodeURIComponent(n.id), text: "Open " + n.type })));
        panel.appendChild(h("div", null, linkList("Uses", n.out.map(function (l) { return { rel: l.rel, title: l.n.title, id: l.n.id }; }), function (l) { view.select(l.id); })));
        panel.appendChild(h("div", null, linkList("Used by", n.inc.map(function (l) { return { rel: l.rel, title: l.n.title, id: l.n.id }; }), function (l) { view.select(l.id); })));
      });
      if (focus && view) view.select(focus);
    });
  }

  // ---- knowledge ------------------------------------------------------------------
  // Memory vaults managed in place, the learnings inbox, the graph, and health.
  var KTABS = [["notes", "Notes"], ["learnings", "Learnings"], ["graph", "Graph"], ["health", "Health"]];
  function csv(s) { return String(s || "").split(",").map(function (x) { return x.trim(); }).filter(Boolean); }
  function pageKnowledge(m, arg) {
    var parts = String(arg || "").split("/");
    var tab = KTABS.some(function (t) { return t[0] === parts[0]; }) ? parts[0] : "notes";
    var want = parts.length > 1 && parts[1] !== "" ? Number(parts[1]) : null;
    var noteId = parts.length > 2 ? parts.slice(2).join("/") : null;
    var ro = S.ov.read_only;
    m.appendChild(header("Knowledge", "What agents know: the memory vaults they read and write, and the learnings they propose for you to review."));
    var top = h("div"), tabsBox = h("div"), body = h("div", { class: "stack" });
    m.appendChild(top); m.appendChild(tabsBox); m.appendChild(body);
    function go(t, i, id) { location.hash = "#/knowledge/" + t + "/" + i + (id ? "/" + encodeURIComponent(id) : ""); }
    api("/api/knowledge" + hq()).then(function (k) {
      var vaults = k.vaults;
      var cur = want !== null && vaults.some(function (v) { return v.index === want; }) ? want
        : ((vaults.find(function (v) { return v.exists && v.notes; }) || vaults.find(function (v) { return v.exists; }) || vaults[0] || { index: 0 }).index);
      var v = vaults.find(function (x) { return x.index === cur; });
      if (vaults.length) {
        top.appendChild(h("div", { class: "vaults", role: "list", "aria-label": "Memory vaults" }, vaults.map(function (x) {
          var pick = function () { go(tab === "learnings" ? "notes" : tab, x.index); };
          return h("div", { class: "vault" + (x.index === cur ? " sel" : ""), role: "listitem", tabindex: "0", title: x.path,
            onclick: pick, onkeydown: function (e) { if (e.key === "Enter") pick(); } },
            h("span", { class: "vlabel", text: x.global ? "Global memory (every agent)" : vaults.length > 1 ? "Vault " + (x.index + 1) : "Vault" }),
            h("code", { class: "vpath", text: x.path }),
            x.exists ? h("span", { class: "vstats" },
              h("span", null, h("b", { text: String(x.notes) }), " notes"),
              h("span", null, h("b", { text: String(x.links) }), " links"),
              h("span", { class: x.dangling ? "warn" : "" }, h("b", { text: String(x.dangling) }), " dangling"))
              : h("span", { class: "vstats" }, h("span", { class: "muted", text: "not created yet" }), ro ? null : h("button", { type: "button", onclick: function (e) {
                e.stopPropagation();
                api("/api/knowledge/init", { method: "POST", body: { h: S.h, i: x.index } }).then(function () { toast("Vault created"); go("notes", x.index); route(); })
                  .catch(function (err) { toast(err.message, true); });
              } }, "Create it")));
        })));
      } else {
        top.appendChild(h("div", { class: "note", text: "This harness names no memory vaults. Add memory = [\"~/notes\"] (or \"{target}/.memory\") to it in Settings > Harnesses. Learnings can still be collected below." }));
      }
      tabsBox.appendChild(h("nav", { class: "tabs", "aria-label": "Knowledge sections" }, KTABS.map(function (t) {
        return h("a", { href: "#/knowledge/" + t[0] + "/" + cur, class: t[0] === tab ? "on" : "", "aria-current": t[0] === tab ? "page" : null },
          t[1], t[0] === "learnings" && k.learnings ? h("b", { class: "badge warn", text: String(k.learnings) }) : null);
      })));
      if (tab === "learnings") return learnings(vaults, cur);
      if (!v || !v.exists) { body.appendChild(empty(vaults.length ? "Create this vault first." : "No vault to show.")); return; }
      if (tab === "graph") return graph(cur);
      if (tab === "health") return health(cur);
      notes(cur, noteId);
    }).catch(function (e) { body.appendChild(empty(e.message)); });

    function notes(i, id) {
      var search = h("input", { type: "search", placeholder: "Search notes", "aria-label": "Search notes", value: store("indra.kq") || "" });
      var kind = h("select", { "aria-label": "Type" }, h("option", { value: "", text: "Any type" }));
      var list = h("div", { class: "klist" }), detail = h("div");
      var exp = function (fmt) { return "/api/knowledge/export" + hq({ i: i, format: fmt }); };
      var bar = h("div", { class: "row" }, search, kind, ro ? null : h("button", { type: "button", class: "primary", onclick: function () { newNote(); } }, "New note"),
        ro ? null : h("button", { type: "button", onclick: function () { importNotes(); } }, "Import"),
        h("details", { class: "menu" }, h("summary", { class: "btn" }, "Export"), h("div", { class: "menubox" },
          h("a", { href: exp("bundle"), download: "" }, "indra bundle (.json, to import elsewhere)"),
          h("a", { href: exp("html"), download: "" }, "Graph as an offline page (.html)"),
          h("a", { href: exp("json"), download: "" }, "Graph as JSON"),
          h("a", { href: exp("mermaid"), download: "" }, "Mermaid diagram"),
          h("a", { href: exp("dot"), download: "" }, "Graphviz DOT"))));
      body.appendChild(bar);
      body.appendChild(h("div", { class: "kgrid" }, list, detail));
      var titles = h("datalist", { id: "note-titles" }); body.appendChild(titles);
      var t = null;
      search.addEventListener("input", function () { clearTimeout(t); t = setTimeout(load, 200); });
      kind.addEventListener("change", load);
      function load() {
        store("indra.kq", search.value);
        api("/api/knowledge/notes" + hq({ i: i, q: search.value || null, type: kind.value || null })).then(function (r) {
          if (kind.options.length === 1) r.types.forEach(function (x) { kind.appendChild(h("option", { value: x, text: x })); });
          list.textContent = ""; titles.textContent = "";
          r.notes.forEach(function (n) { titles.appendChild(h("option", { value: n.title })); });
          if (!r.notes.length) { list.appendChild(empty(search.value ? "Nothing matches." : "No notes yet.")); return; }
          r.notes.forEach(function (n) {
            list.appendChild(h("a", { class: "kitem" + (n.id === id ? " sel" : ""), href: "#/knowledge/notes/" + i + "/" + encodeURIComponent(n.id) },
              h("div", { class: "row" }, h("b", { text: n.title }), n.type ? h("span", { class: "pill", text: n.type }) : null),
              n.snippet ? h("div", { class: "muted small", text: n.snippet }) : null,
              h("div", { class: "muted small", text: [n.tags.map(function (x) { return "#" + x; }).join(" "), n.links + " links", n.updated || ""].filter(Boolean).join(" | ") })));
          });
        }).catch(function (e) { list.textContent = ""; list.appendChild(empty(e.message)); });
      }
      load();
      if (id) show(id); else detail.appendChild(empty("Pick a note to read or edit it."));
      function show(nid) {
        api("/api/knowledge/note" + hq({ i: i, id: nid })).then(function (d) {
          detail.textContent = "";
          var ta = h("textarea", { class: "notetext", "aria-label": "Note text", spellcheck: "false" }); ta.value = d.text;
          var hash = d.hash;
          var rels = d.relations.filter(function (r) { return r.dir === "out"; }), back = d.relations.filter(function (r) { return r.dir === "in"; });
          function relList(title, xs) {
            return [h("h3", { text: title }), xs.length ? h("ul", { class: "links" }, xs.map(function (r) {
              return h("li", null, h("span", { class: "rel", text: r.rel }), h("a", { href: "#/knowledge/notes/" + i + "/" + encodeURIComponent(r.note.id), text: r.note.title }));
            })) : h("p", { class: "muted", text: "none" })];
          }
          var to = h("input", { type: "text", list: "note-titles", placeholder: "another note", "aria-label": "Link to" });
          var rel = h("input", { type: "text", value: "related", "aria-label": "Relation", size: "12" });
          detail.appendChild(h("div", { class: "card" },
            h("div", { class: "row" }, h("h2", { text: d.title }), d.type ? h("span", { class: "pill", text: d.type }) : null),
            h("p", { class: "muted small", text: [d.tags.map(function (x) { return "#" + x; }).join(" "), d.source ? "source: " + d.source : "", d.updated ? "updated " + d.updated : ""].filter(Boolean).join(" | ") }),
            h("div", { class: "split2" }, h("div", null, relList("Links out", rels)), h("div", null, relList("Backlinks", back))),
            d.unresolved.length ? h("p", { class: "muted small", text: "Unresolved: " + d.unresolved.join(", ") }) : null,
            ro ? null : h("form", { class: "row", onsubmit: function (e) {
              e.preventDefault();
              if (!to.value.trim()) return;
              api("/api/knowledge/link", { method: "POST", body: { h: S.h, i: i, from: d.id, to: to.value.trim(), rel: rel.value.trim() || "related" } })
                .then(function (r) { toast(r.changed ? "Linked" : "Already linked"); show(d.id); load(); }).catch(function (err) { toast(err.message, true); });
            } }, h("span", { class: "muted", text: "Link to" }), to, h("span", { class: "muted", text: "as" }), rel, h("button", { type: "submit" }, "Link")),
            h("h3", { text: "Text" }), ta,
            h("p", { class: "muted small", text: d.path }),
            h("div", { class: "row" },
              ro ? null : h("button", { type: "button", class: "primary", onclick: function () {
                api("/api/knowledge/note", { method: "PUT", body: { h: S.h, i: i, id: d.id, text: ta.value, hash: hash } })
                  .then(function (r) { hash = r.hash; toast("Saved"); show(d.id); load(); }).catch(function (err) { toast(err.message, true); });
              } }, "Save"),
              h("button", { type: "button", onclick: function () { saveFile(d.id.split("/").pop() + ".md", ta.value, "text/markdown"); } }, "Download .md"),
              ro ? null : h("button", { type: "button", class: "danger", onclick: function () {
                if (!window.confirm("Delete the note \"" + d.title + "\"? Links to it become unresolved.")) return;
                api("/api/knowledge/note" + hq({ i: i, id: d.id }), { method: "DELETE" }).then(function () { toast("Deleted"); go("notes", i); })
                  .catch(function (err) { toast(err.message, true); });
              } }, "Delete"))));
        }).catch(function (e) { detail.textContent = ""; detail.appendChild(empty(e.message)); });
      }
      function importNotes() {
        var files = [], from = h("select", { "aria-label": "Source" }, [["auto", "Detect"], ["markdown", "Markdown folder"], ["obsidian", "Obsidian vault"], ["notion", "Notion export (unzipped)"], ["github-wiki", "GitHub wiki (cloned)"]]
          .map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
        var folder = h("input", { type: "text", placeholder: "optional, e.g. imported/notion" }), tags = h("input", { type: "text", placeholder: "optional, comma separated" });
        var overwrite = h("input", { type: "checkbox" });
        var picked = h("p", { class: "picked muted", text: "Nothing picked yet." }), out = h("div");
        var md = function (n) { return /\.(md|markdown|json)$/i.test(n); };
        function take(list) {
          files = []; var bundles = 0;
          list.forEach(function (f) {
            if (/\.json$/i.test(f.name)) {
              try { var b = JSON.parse(f.text); if (b && b.format === "indra-vault") { bundles++; b.notes.forEach(function (n) { files.push({ path: n.path, text: n.text }); }); } } catch (e) { /* not a bundle */ }
            } else files.push({ path: f.path, text: f.text });
          });
          if (bundles || (from.value === "auto" && (list.all || []).some(function (p) { return /(^|\/)\.obsidian\//.test(p); }))) from.value = "obsidian";
          picked.textContent = files.length + " note file(s)" + (bundles ? " from " + bundles + " bundle(s)" : "") + (list.skipped ? ", " + list.skipped + " other file(s) left out" : "") + ".";
          go.disabled = preview.disabled = !files.length;
        }
        function run(dry) {
          go.disabled = preview.disabled = true; out.textContent = dry ? "Checking..." : "Importing...";
          api("/api/knowledge/import", { method: "POST", body: { h: S.h, i: i, files: files, from: from.value, folder: folder.value.trim() || null, tags: csv(tags.value), overwrite: overwrite.checked, dry_run: dry } })
            .then(function (r) {
              out.textContent = "";
              out.appendChild(h("div", { class: "note " + (dry ? "" : "ok") },
                h("b", { text: (dry ? "Would import " : "Imported ") + r.imported.length + " note(s)" + (r.from ? " as " + r.from : "") + "." }),
                h("div", { text: "Links converted: " + r.links_converted + ". Skipped (exist already): " + r.skipped_existing.length + ". Unresolved links: " + r.unresolved.length + "." }),
                r.skipped_existing.length ? h("div", { class: "muted small", text: "Existing: " + r.skipped_existing.slice(0, 12).join(", ") + (r.skipped_existing.length > 12 ? "..." : "") + " (tick Replace to overwrite them)" }) : null));
              if (!dry) { toast("Imported " + r.imported.length + " note(s)"); load(); } else { go.disabled = preview.disabled = false; }
            }).catch(function (e) { out.textContent = ""; out.appendChild(h("div", { class: "note bad", text: e.message })); go.disabled = preview.disabled = false; });
        }
        var preview = h("button", { type: "button", disabled: true, onclick: function () { run(true); } }, "Preview");
        var go = h("button", { type: "button", class: "primary", disabled: true, onclick: function () { run(false); } }, "Import");
        detail.textContent = "";
        detail.appendChild(h("div", { class: "card form importbox" }, h("h3", { text: "Import notes" }),
          h("p", { class: "muted small", text: "Pick Markdown files, a whole folder (an Obsidian vault, an unzipped Notion export, a cloned GitHub wiki), or an indra bundle exported from another vault. Links are converted to [[wikilinks]]; existing notes are kept unless you tick Replace." }),
          h("div", { class: "row" },
            h("button", { type: "button", onclick: function () { pickFiles({ multiple: true, accept: ".md,.markdown,.json", keep: md }).then(take); } }, "Choose files"),
            h("button", { type: "button", onclick: function () { pickFiles({ folder: true, keep: md }).then(take); } }, "Choose a folder")),
          picked,
          h("div", { class: "row" }, h("label", null, "Source", from), h("label", null, "Into folder", folder), h("label", null, "Add tags", tags)),
          h("label", { class: "check" }, overwrite, h("span", { text: "Replace notes that exist already" })),
          h("div", { class: "row" }, preview, go, h("button", { type: "button", onclick: function () { go_back(); } }, "Close")), out));
        function go_back() { detail.textContent = ""; if (noteId) show(noteId); else detail.appendChild(empty("Pick a note to read or edit it.")); }
      }
      function newNote() {
        var f = { title: h("input", { type: "text", required: true }), type: h("input", { type: "text", list: "note-types", placeholder: "learning, pattern, finding, decision..." }),
          tags: h("input", { type: "text", placeholder: "comma separated" }), links: h("input", { type: "text", placeholder: "titles of notes it relates to, comma separated" }),
          body: h("textarea", { placeholder: "What should an agent know?" }) };
        detail.textContent = "";
        detail.appendChild(h("form", { class: "form card", onsubmit: function (e) {
          e.preventDefault();
          api("/api/knowledge/notes", { method: "POST", body: { h: S.h, i: i, title: f.title.value, type: f.type.value, tags: csv(f.tags.value), links: csv(f.links.value), body: f.body.value } })
            .then(function (r) { toast("Created " + r.id); go("notes", i, r.id); }).catch(function (err) { toast(err.message, true); });
        } }, h("h3", { text: "New note" }), h("label", null, "Title", f.title), h("label", null, "Type", f.type),
          h("datalist", { id: "note-types" }, ["learning", "pattern", "finding", "technique", "tool", "decision"].map(function (x) { return h("option", { value: x }); })),
          h("label", null, "Tags", f.tags), h("label", null, "Links", f.links), h("label", null, "Text", f.body),
          h("div", { class: "row" }, h("button", { class: "primary", type: "submit" }, "Create"), h("button", { type: "button", onclick: function () { go("notes", i, noteId); route(); } }, "Cancel"))));
        f.title.focus();
      }
    }

    function learnings(vaults, cur) {
      var holder = h("div", { class: "stack" });
      body.appendChild(h("p", { class: "muted", text: "Agents launched with \"Ask for learnings\" (or a profile with reflect = true) propose what they learned before they finish. Nothing reaches memory until you accept it: it then becomes a learning note, with where it came from, in the global memory unless you pick another vault, so every agent on every project can be given it when it is relevant." }));
      body.appendChild(holder);
      var existing = vaults.filter(function (v) { return true; });
      function vaultSelect() {
        var s = h("select", { "aria-label": "Vault" }, existing.map(function (v) { return h("option", { value: String(v.index), text: (v.global ? "Global memory (every agent): " : "") + v.path + (v.exists ? "" : " (created on accept)") }); }));
        // Learnings go to the global memory unless you pick another vault.
        var g = existing.find(function (v) { return v.global; });
        s.value = String(g ? g.index : cur);
        return s;
      }
      function load() {
        api("/api/learnings").then(function (list) {
          holder.textContent = "";
          var open = list.filter(function (l) { return l.status === "proposed"; }), done = list.filter(function (l) { return l.status !== "proposed"; });
          holder.appendChild(h("div", { class: "row" },
            h("button", { type: "button", disabled: !list.length, onclick: function () {
              jsonFile("learnings.json", list.map(function (l) { return { title: l.title, body: l.body, tags: l.tags, status: l.status, origin: l.origin, note: l.note || null }; }));
            } }, "Export"),
            ro ? null : h("button", { type: "button", onclick: function () {
              pickFiles({ accept: ".json,application/json" }).then(function (fs) {
                if (!fs.length) return;
                var items; try { items = listFrom(fs[0].text); } catch (e) { toast(fs[0].name + ": " + e.message, true); return; }
                api("/api/learnings/import", { method: "POST", body: { items: items.map(function (x) { return { title: String(x.title || ""), body: String(x.body || ""), tags: x.tags || [] }; }) } })
                  .then(function (r) { toast("Added " + r.added + " learning(s) to the inbox"); load(); refreshOverview(); }).catch(function (e) { toast(e.message, true); });
              });
            } }, "Import"),
            done.length && !ro ? h("button", { type: "button", onclick: function () {
              if (!window.confirm("Remove the " + done.length + " accepted learning(s) from this list? Their notes stay in memory.")) return;
              Promise.all(done.map(function (l) { return api("/api/learnings/" + l.id, { method: "DELETE" }); })).then(load).catch(function (e) { toast(e.message, true); });
            } }, "Clear accepted") : null));
          if (!ro) holder.appendChild(addForm());
          if (!open.length) holder.appendChild(empty("Nothing waiting for review."));
          open.forEach(function (l) { holder.appendChild(card(l)); });
          if (done.length) holder.appendChild(h("details", { class: "card", keep: "learnings-accepted" }, h("summary", { text: "Accepted (" + done.length + ")" }),
            h("ul", { class: "links" }, done.map(function (l) {
              var vi = vaults.findIndex(function (v) { return l.note && v.path === l.note[0]; });
              return h("li", null, h("span", { class: "rel", text: ago(l.created) }),
                vi >= 0 ? h("a", { href: "#/knowledge/notes/" + vaults[vi].index + "/" + encodeURIComponent(l.note[1]), text: l.title }) : h("span", { text: l.title }),
                ro ? null : h("button", { type: "button", class: "ghost small", title: "Remove from this list (the note stays)", onclick: function () {
                  api("/api/learnings/" + l.id, { method: "DELETE" }).then(load).catch(function (e) { toast(e.message, true); });
                } }, "Remove"));
            }))));
        }).catch(function (e) { holder.textContent = ""; holder.appendChild(empty(e.message)); });
      }
      function origin(o) {
        var bits = [o.by === "agent" ? "proposed by the agent" : "added by a person"];
        if (o.runner) bits.push(o.runner + (o.profile ? " (" + o.profile + ")" : ""));
        if (o.target) bits.push("in " + o.target);
        return h("p", { class: "muted small" }, o.proc ? h("a", { href: "#/proc/" + o.proc, text: "process #" + o.proc }) : null, (o.proc ? ", " : "") + bits.join(", "));
      }
      function card(l) {
        var title = h("input", { type: "text", value: l.title, "aria-label": "Title" });
        var text = h("textarea", { "aria-label": "Learning" }); text.value = l.body;
        var tags = h("input", { type: "text", value: l.tags.join(", "), "aria-label": "Tags", placeholder: "tags, comma separated" });
        var links = h("input", { type: "text", list: "note-titles", placeholder: "link to notes (titles, comma separated)", "aria-label": "Links" });
        var vs = vaultSelect();
        function patch() { return api("/api/learnings/" + l.id, { method: "PATCH", body: { title: title.value, body: text.value, tags: csv(tags.value) } }); }
        return h("div", { class: "card form learning" }, origin(l.origin),
          h("label", null, "Title", title), h("label", null, "What it learned", text), h("label", null, "Tags", tags),
          ro ? null : h("div", { class: "fields" }, h("label", null, "Into vault", vs), h("label", null, "Link to notes", links)),
          ro ? null : h("div", { class: "row" },
            h("button", { type: "button", class: "primary", onclick: function () {
              patch().then(function () { return api("/api/learnings/" + l.id + "/accept", { method: "POST", body: { h: S.h, i: Number(vs.value), links: csv(links.value) } }); })
                .then(function (r) { toast((r.note.created ? "Saved as " : "Merged into ") + r.note.id); load(); refreshOverview(); })
                .catch(function (err) { toast(err.message, true); });
            } }, "Accept into memory"),
            h("button", { type: "button", onclick: function () { patch().then(function () { toast("Saved"); }).catch(function (err) { toast(err.message, true); }); } }, "Save edits"),
            h("button", { type: "button", class: "danger", onclick: function () {
              if (!window.confirm("Discard this learning? It is deleted, not added to memory.")) return;
              api("/api/learnings/" + l.id, { method: "DELETE" }).then(function () { load(); refreshOverview(); }).catch(function (err) { toast(err.message, true); });
            } }, "Discard")));
      }
      function addForm() {
        var t = h("input", { type: "text", placeholder: "Add a learning yourself: a short title", "aria-label": "New learning title" });
        var b = h("textarea", { placeholder: "What should future agents know?", "aria-label": "New learning", rows: "2" });
        return h("details", { class: "card" }, h("summary", { text: "Add a learning" }), h("form", { class: "form", onsubmit: function (e) {
          e.preventDefault();
          api("/api/learnings", { method: "POST", body: { title: t.value, body: b.value } }).then(function () { load(); refreshOverview(); }).catch(function (err) { toast(err.message, true); });
        } }, t, b, h("div", null, h("button", { type: "submit" }, "Add to the inbox"))));
      }
      body.appendChild(h("datalist", { id: "note-titles" }));
      load();
    }

    function graph(i) {
      api("/api/memory/graph" + hq({ i: i })).then(function (g) {
        var gv = graphView(body, g, function (n, panel) {
          panel.appendChild(h("h3", { text: n.type }));
          panel.appendChild(h("h2", { text: n.title }));
          if (n.raw.tags && n.raw.tags.length) panel.appendChild(h("div", { class: "chips" }, n.raw.tags.map(function (t) { return h("span", { class: "pill", text: "#" + t }); })));
          panel.appendChild(h("a", { href: "#/knowledge/notes/" + i + "/" + encodeURIComponent(n.id), text: "Open and edit" }));
          if (n.raw.body) panel.appendChild(h("pre", { text: n.raw.body.trim() }));
          panel.appendChild(h("div", null, linkList("Links out", n.out.map(function (l) { return { rel: l.rel, title: l.n.title, id: l.n.id }; }), function (l) { gv.select(l.id); })));
          panel.appendChild(h("div", null, linkList("Backlinks", n.inc.map(function (l) { return { rel: l.rel, title: l.n.title, id: l.n.id }; }), function (l) { gv.select(l.id); })));
        });
      }).catch(function (e) { body.appendChild(empty(e.message)); });
    }

    function health(i) {
      api("/api/knowledge/health" + hq({ i: i })).then(function (r) {
        var link = function (id, title) { return h("a", { href: "#/knowledge/notes/" + i + "/" + encodeURIComponent(id), text: title }); };
        body.appendChild(h("div", { class: "card" }, h("h3", { text: "Dangling links (" + r.dangling.length + ")" }),
          h("p", { class: "muted small", text: "Links to notes that do not exist. Create the note, or fix the link." }),
          r.dangling.length ? h("ul", { class: "links" }, r.dangling.map(function (d) {
            return h("li", null, link(d.from, d.title), h("span", { class: "rel", text: " -" + d.rel + "-> " }), h("span", { text: d.target }));
          })) : h("p", { class: "muted", text: "none" })));
        body.appendChild(h("div", { class: "card" }, h("h3", { text: "Orphans (" + r.orphans.length + ")" }),
          h("p", { class: "muted small", text: "Notes nothing links to and that link nowhere: recall reaches them only by search. Link them to related notes." }),
          r.orphans.length ? h("ul", { class: "links" }, r.orphans.map(function (o) { return h("li", null, link(o.id, o.title)); })) : h("p", { class: "muted", text: "none" })));
      }).catch(function (e) { body.appendChild(empty(e.message)); });
    }
  }

  function segs(stages) { return h("div", { class: "segs", title: stages.map(function (s) { return s.name + ": " + s.state; }).join(", ") }, stages.map(function (s) { return h("i", { class: s.state }); })); }

  // A workflow's inputs as a form; `done(run)` gets the started run.
  function workflowForm(d, done) {
    var ro = S.ov.read_only, ins = d.input || [];
    var hs = (S.ov.harnesses || []).find(function (x) { return x.name === d.harness; }) || harness();
    var ctl = ins.map(function (i) {
      var kind = i.kind || "text", el, label = i.label || i.name;
      if (kind === "choice") {
        el = h("select", { "aria-label": label }, (i.options || []).map(function (o) { return h("option", { value: o.value, text: o.label }); }));
        if (i.default != null) el.value = i.default;
      } else if (kind === "bool") {
        el = h("input", { type: "checkbox" }); el.checked = /^(true|1|yes|on)$/.test(i.default || "");
      } else if (kind === "target") {
        var ts = hs ? hs.targets : [];
        el = h("select", { "aria-label": label }, (i.required ? [] : [h("option", { value: "", text: "none" })]).concat(ts.map(function (t) { return h("option", { value: t, text: t }); })));
      } else if (kind === "profile") {
        el = h("select", { "aria-label": label }, [h("option", { value: "", text: "none (the runner's defaults)" })].concat((S.ov.profiles || []).map(function (p) {
          return h("option", { value: p.name, text: p.name + (p.provider === "ollama" ? "  (Ollama: " + (p.model || "?") + ")" : p.description ? "  -  " + p.description : "") });
        })));
      } else {
        el = h("input", { type: "text", placeholder: i.placeholder || "", "aria-label": label });
        if (i.default) el.value = i.default;
        if (i.required) el.required = true;
      }
      var field = kind === "bool" ? h("label", { class: "check" }, el, h("span", { text: label }))
        : h("label", null, label + (i.required ? " *" : ""), i.help ? h("small", { text: i.help }) : null, el);
      return { i: i, kind: kind, el: el, field: field };
    });
    var go = h("button", { type: "submit", class: "primary", disabled: ro }, "Run " + d.name);
    var form = h("form", { class: "form wfrun", onsubmit: function (e) {
      e.preventDefault(); go.disabled = true;
      var inputs = {};
      ctl.forEach(function (c) { inputs[c.i.name] = c.kind === "bool" ? String(c.el.checked) : c.el.value; });
      api("/api/workflows", { method: "POST", body: { name: d.name, inputs: inputs } }).then(function (r) {
        toast("Started " + d.name); if (done) done(r); else location.hash = "#/workflow/" + r.id;
      }).catch(function (err) { toast(err.message, true); go.disabled = false; });
    } });
    var plain = ctl.filter(function (c) { return c.kind !== "bool"; }), checks = ctl.filter(function (c) { return c.kind === "bool"; });
    for (var k = 0; k < plain.length; k += 3) form.appendChild(h("div", { class: "fields" }, plain.slice(k, k + 3).map(function (c) { return c.field; })));
    checks.forEach(function (c) { form.appendChild(c.field); });
    if (ro) form.appendChild(h("p", { class: "muted small", text: "indra is running read-only." }));
    form.appendChild(h("div", { class: "row" }, go));
    return form;
  }

  // Runs page: run any workflow, its inputs filled in here.
  function runWorkflowCard() {
    if (S.ov.read_only) return null;
    var box = h("div"), sel = h("select", { "aria-label": "Workflow" }), body = h("div");
    var card = h("details", { class: "card prreview", open: true }, h("summary", { text: "Run a workflow" }),
      h("p", { class: "muted small", text: "A pull request review, a full security review, or any workflow from Settings > Workflows. Each runs with the profile you pick (any runner and model, a local one included); its run appears below." }),
      h("div", { class: "row" }, sel), body);
    api("/api/workflows").then(function (w) {
      var defs = w.defs || [];
      if (!defs.length) { box.remove(); return; }
      defs.forEach(function (d) { sel.appendChild(h("option", { value: d.name, text: d.name + (d.description ? "  -  " + d.description : "") })); });
      var start = defs.find(function (d) { return d.name === "pr-review"; }) || defs[0];
      sel.value = start.name;
      function show() {
        var d = defs.find(function (x) { return x.name === sel.value; });
        body.textContent = "";
        body.appendChild(workflowForm(d));
      }
      sel.addEventListener("change", show); show();
      box.appendChild(card);
    }).catch(function () { box.remove(); });
    return box;
  }

  function pageRuns(m) {
    var hs = harness();
    m.appendChild(header("Results", hs && hs.has_runs ? "Run directories under " + hs.targets.join(", ") + ". Refreshes every 3 seconds." : "This harness does not define a runs directory."));
    var rwc = runWorkflowCard(); if (rwc) m.appendChild(rwc);
    var holder = h("div", { class: "tablewrap" });
    m.appendChild(holder);
    function load() {
      api("/api/runs" + hq()).then(function (runs) {
        holder.textContent = "";
        if (!runs.length) { holder.appendChild(empty("No results yet. They appear here when an agent or a review writes them: start one with Run an agent.")); return; }
        holder.appendChild(h("table", null,
          h("thead", null, h("tr", null, ["Run", "Progress", "Results", "By agent", "Updated"].map(function (t) { return h("th", { text: t }); }))),
          h("tbody", null, runs.map(function (r) {
            var go = function () { location.hash = "#/run/" + encodeURIComponent(r.key); };
            var groups = Object.keys(r.counts.by_group).map(function (g) { return g + " " + r.counts.by_group[g]; }).join(", ");
            return h("tr", { class: "click", tabindex: "0", onclick: go, onkeydown: function (e) { if (e.key === "Enter") go(); } },
              h("td", null, h("b", { text: r.name }), " ", r.active ? h("span", { class: "pill run live", text: "live" }) : null, h("div", { class: "muted", text: r.target })),
              h("td", null, segs(r.stages)),
              h("td", { text: String(r.counts.total) }),
              h("td", { class: "desc", text: groups || "-" }),
              h("td", { class: "muted", text: ago(r.modified) }));
          }))));
      }).catch(function (e) { holder.textContent = ""; holder.appendChild(empty(e.message)); });
    }
    load(); every(3000, load);
  }

  function bars(obj) {
    var keys = Object.keys(obj).sort(function (a, b) { return obj[b] - obj[a]; });
    if (!keys.length) return h("p", { class: "muted", text: "none yet" });
    var max = Math.max.apply(null, keys.map(function (k) { return obj[k]; }));
    return h("div", { class: "bars" }, keys.map(function (k) {
      var fill = h("div", { class: "fill" }); fill.style.width = (100 * obj[k] / max) + "%";
      return h("div", { class: "bar" }, h("span", { text: k }), h("div", { class: "track" }, fill), h("b", { text: String(obj[k]) }));
    }));
  }

  // A run's findings: change a status (with the reason), and compare with another run.
  var SEV_ORDER = { critical: 0, high: 1, medium: 2, low: 3, info: 4 };
  // A run's threat model (threat-model.json, written by sh-threat-modeler):
  // its trust boundaries with their controls, and its threats ranked by risk.
  var STRIDE = { S: "Spoofing", T: "Tampering", R: "Repudiation", I: "Information disclosure", D: "Denial of service", E: "Elevation of privilege" };
  function loc(l) { return l && l.file ? l.file + (l.line ? ":" + l.line : "") : ""; }
  function runThreats(key) {
    var box = h("div");
    api("/api/run/file" + hq({ key: key, rel: "threat-model.json", max: 4194304 })).then(function (f) {
      var tm;
      try { tm = JSON.parse(f.text); } catch (e) { return; }
      if (!tm || typeof tm !== "object") return;
      var threats = Array.isArray(tm.threats) ? tm.threats : [], bounds = Array.isArray(tm.boundaries) ? tm.boundaries : [];
      var bname = {}; bounds.forEach(function (b) { bname[b.id] = b.name || b.id; });
      var open = threats.filter(function (t) { return t.status === "open"; });
      var card = h("div", { class: "card tm" });
      card.appendChild(h("h3", { text: "Threat model" }));
      card.appendChild(h("p", { class: "small", text: bounds.length + " trust boundaries, " + threats.length + " threats, " + open.length + " open" + (tm.mode && tm.mode !== "code" ? " (" + tm.mode + ")" : "") }));
      if (tm.summary) card.appendChild(h("p", { text: String(tm.summary) }));
      if (bounds.length) {
        card.appendChild(h("h4", { text: "Trust boundaries" }));
        card.appendChild(h("div", { class: "tablewrap" }, h("table", null,
          h("thead", null, h("tr", null, ["Boundary", "Controls", "Gaps"].map(function (x) { return h("th", { text: x }); }))),
          h("tbody", null, bounds.map(function (b) {
            var ctl = (Array.isArray(b.controls) ? b.controls : []).map(function (c) { return (c.name || "") + (loc(c) ? " (" + loc(c) + ")" : ""); });
            var gaps = Array.isArray(b.gaps) ? b.gaps : [];
            return h("tr", null, h("td", null, h("b", { text: b.name || b.id || "" })),
              h("td", { class: "small", text: ctl.length ? ctl.join("; ") : "none found" }),
              h("td", { class: "small" + (gaps.length ? " warn-text" : ""), text: gaps.join("; ") }));
          })))));
      }
      if (threats.length) {
        var show = h("select", { "aria-label": "Show threats" }, [["open", "open"], ["all", "all"]].map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
        var tbody = h("tbody");
        var rank = function (t) { var r = SEV_ORDER[t.risk]; return r == null ? 9 : r; };
        var rows = function () {
          tbody.textContent = "";
          threats.slice().sort(function (a, b) { return rank(a) - rank(b); })
            .filter(function (t) { return show.value === "all" || t.status === "open"; })
            .forEach(function (t) {
              var more = h("div", { class: "small muted" },
                t.scenario ? h("div", { text: String(t.scenario) }) : null,
                t.check ? h("div", null, h("b", { text: "Check: " }), String(t.check)) : null,
                Array.isArray(t.locations) && t.locations.length ? h("div", { class: "mono", text: t.locations.map(loc).filter(Boolean).join("  ") }) : null);
              tbody.appendChild(h("tr", null,
                h("td", null, h("span", { class: "sev sev-" + (t.risk || "info"), text: t.risk || "?" })),
                h("td", null, h("b", { text: (t.id ? t.id + ": " : "") + (t.title || "") }), more),
                h("td", { class: "small", title: STRIDE[t.stride] || "", text: STRIDE[t.stride] || t.stride || "" }),
                h("td", { class: "small", text: bname[t.boundary] || t.boundary || "" }),
                h("td", null, h("span", { class: "pill" + (t.status === "open" ? " warn" : ""), text: t.status || "" })),
                h("td", { class: "small", text: Array.isArray(t.classes) ? t.classes.join(", ") : "" })));
            });
          if (!tbody.childNodes.length) tbody.appendChild(h("tr", null, h("td", { colspan: "6", class: "muted", text: "No open threats." })));
        };
        show.addEventListener("change", rows);
        card.appendChild(h("div", { class: "row" }, h("h4", { text: "Threats" }), h("label", { class: "small" }, "Show ", show)));
        card.appendChild(h("div", { class: "tablewrap" }, h("table", null,
          h("thead", null, h("tr", null, ["Risk", "Threat", "STRIDE", "Boundary", "Status", "Hunt"].map(function (x) { return h("th", { text: x }); }))), tbody)));
        rows();
      }
      card.appendChild(h("p", { class: "muted small", text: "The diagram and the full write-up are in threat-model.md below." }));
      box.appendChild(card);
    }).catch(function () { /* no threat model in this run */ });
    return box;
  }

  function runFindings(key) {
    var box = h("div");
    api("/api/run/findings" + hq({ key: key })).then(function (r) {
      var list = r.findings || [];
      if (!list.length) return;
      var ro = S.ov.read_only;
      var card = h("div", { class: "card findings" });
      var filter = h("select", { "aria-label": "Show" }, [["open", "open (not false positives)"], ["all", "all"], ["false-positive", "false positives"], ["needs-runtime", "needs runtime"]].map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
      var tbody = h("tbody");
      function rows() {
        tbody.textContent = "";
        list.slice().sort(function (a, b) { return (SEV_ORDER[a.severity] == null ? 9 : SEV_ORDER[a.severity]) - (SEV_ORDER[b.severity] == null ? 9 : SEV_ORDER[b.severity]); })
          .filter(function (f) { return filter.value === "all" || (filter.value === "open" ? f.status !== "false-positive" : f.status === filter.value); })
          .forEach(function (f) {
            var st = h("select", { "aria-label": "Status of " + (f.id || f.title) }, ["verified", "needs-runtime", "false-positive"].concat(["verified", "needs-runtime", "false-positive"].indexOf(f.status) < 0 && f.status ? [f.status] : [])
              .map(function (x) { return h("option", { value: x, text: x }); }));
            st.value = f.status;
            var why = h("input", { type: "text", placeholder: "why (required for a false positive)", "aria-label": "Reason" });
            var save = h("button", { type: "button", disabled: true }, "Save");
            function dirty() { save.disabled = st.value === f.status && !why.value.trim(); }
            st.addEventListener("change", dirty); why.addEventListener("input", dirty);
            save.addEventListener("click", function () {
              save.disabled = true;
              api("/api/run/triage", { method: "POST", body: { h: S.h, key: key, index: f.index, fingerprint: f.fingerprint, status: st.value, reason: why.value } }).then(function (n) {
                list[list.indexOf(f)] = n; toast((n.id || "Finding") + ": " + n.status + (n.status === "false-positive" ? ". Record into memory to remember it." : "")); rows();
              }).catch(function (e) { toast(e.message, true); dirty(); });
            });
            var t = f.triage;
            tbody.appendChild(h("tr", null,
              h("td", null, h("span", { class: "sev sev-" + (f.severity || "info"), text: f.severity || "?" })),
              h("td", null, h("b", { text: (f.id ? f.id + ": " : "") + (f.title || "(untitled)") }),
                h("div", { class: "muted small", text: [f.class, f.file ? f.file + (f.line != null ? ":" + f.line : "") : ""].filter(Boolean).join("  ") }),
                t && t.to ? h("div", { class: "muted small", text: "triaged " + t.from + " -> " + t.to + (t.reason ? ": " + t.reason : "") }) : null),
              h("td", null, ro ? h("span", { text: f.status }) : st),
              ro ? null : h("td", { class: "grow" }, h("div", { class: "row" }, why, save))));
          });
        if (!tbody.childNodes.length) tbody.appendChild(h("tr", null, h("td", { colspan: "4", class: "muted", text: "None." })));
      }
      filter.addEventListener("change", rows);
      card.appendChild(h("div", { class: "row" }, h("h3", { class: "grow", text: "Findings (" + list.length + ")" }), h("span", { class: "muted small", text: "Show" }), filter));
      card.appendChild(h("div", { class: "tablewrap" }, h("table", null, h("thead", null, h("tr", null, ["", "Finding", "Status", ro ? null : "Change"].filter(function (x) { return x !== null; }).map(function (x) { return h("th", { text: x }); }))), tbody)));
      // Compare with another run of this harness.
      var other = h("select", { "aria-label": "Compare with" }), out = h("div");
      card.appendChild(h("div", { class: "row compare" }, h("b", { text: "Compare with" }), other,
        h("button", { type: "button", onclick: function () {
          if (!other.value) return;
          api("/api/run/compare" + hq({ key: key, with: other.value })).then(function (c) {
            out.textContent = "";
            function group(title, items, cls, fmt) {
              return h("div", { class: "cmpgroup " + cls }, h("h4", { text: title + " (" + items.length + ")" }),
                items.length ? h("ul", { class: "links" }, items.slice(0, 200).map(function (x) { var f = x.finding || x; return h("li", null, h("span", { class: "rel", text: f.severity || "" }), (f.id ? f.id + ": " : "") + (f.title || f.file || f.fingerprint) + (fmt ? fmt(x) : "")); })) : h("p", { class: "muted small", text: "none" }));
            }
            out.appendChild(h("p", { class: "muted small", text: c.head + " against " + c.base + ", matched by fingerprint. False positives count as closed." }));
            out.appendChild(h("div", { class: "cmp" }, group("New", c.new, "new"), group("Fixed", c.fixed, "fixed"), group("Still open", c.still_open, "open"),
              group("Changed status", c.changed, "changed", function (x) { return "  (" + x.from + " -> " + x.to + ")"; })));
          }).catch(function (e) { toast(e.message, true); });
        } }, "Compare")));
      card.appendChild(out);
      api("/api/runs" + hq()).then(function (runs) {
        runs.filter(function (x) { return x.key !== key && x.counts && x.counts.source === "verified.jsonl"; }).forEach(function (x) { other.appendChild(h("option", { value: x.key, text: x.name + "  (" + x.counts.total + " findings, " + ago(x.modified) + ")" })); });
        if (!other.options.length) { other.appendChild(h("option", { value: "", text: "no other run with findings" })); other.disabled = true; }
      });
      rows();
      box.appendChild(card);
    }).catch(function () { /* no verified.jsonl: nothing to triage */ });
    return box;
  }

  // What a finished review run can do next: remember it, publish it, post it.
  function runActions(key) {
    var box = h("div");
    api("/api/run/actions" + hq({ key: key })).then(function (a) {
      if (!a.findings && !a.review) return;
      var ro = S.ov.read_only, out = h("div");
      var card = h("div", { class: "card form runactions" }, h("h3", { text: "Next steps" }));
      function show(title, node) { out.textContent = ""; out.appendChild(h("h4", { text: title })); out.appendChild(node); }
      function pre(text) { return h("pre", { class: "view small", text: text }); }
      if (a.findings) {
        card.appendChild(h("div", { class: "row" },
          h("b", { text: "Memory" }), h("span", { class: "muted small grow", text: "Record the findings and false positives into " + (a.vault || "the run's memory vault") }),
          h("button", { type: "button", onclick: function () {
            api("/api/run/record", { method: "POST", body: { h: S.h, key: key, dry_run: true } }).then(function (r) {
              show("Would run " + r.commands.length + " command(s): " + r.new + " new, " + r["seen-again"] + " seen again, " + r["false-positive"] + " false positive(s)", pre(r.commands.join("\n")));
            }).catch(function (e) { toast(e.message, true); });
          } }, "Preview"),
          ro ? null : h("button", { type: "button", class: "primary", onclick: function (e) {
            e.target.disabled = true;
            api("/api/run/record", { method: "POST", body: { h: S.h, key: key } }).then(function (r) {
              toast("Recorded: " + r.new + " new, " + r["seen-again"] + " seen again, " + r["false-positive"] + " false positive(s)");
              show("Recorded into " + r.vault, pre(JSON.stringify(r, null, 2)));
            }).catch(function (err) { toast(err.message, true); }).then(function () { e.target.disabled = false; });
          } }, "Record into memory")));
        var to = h("select", { "aria-label": "Destination" }, ["jira", "github", "confluence", "notion"].map(function (x) { return h("option", { value: x, text: x }); }));
        var sev = h("select", { "aria-label": "Lowest severity" }, ["info", "low", "medium", "high", "critical"].map(function (x) { return h("option", { value: x, text: x + " and up" }); })); sev.value = "medium";
        var nr = h("input", { type: "checkbox" });
        card.appendChild(h("div", { class: "row" },
          h("b", { text: "Publish" }), to, sev, h("label", { class: "check" }, nr, h("span", { text: "include needs-runtime" })),
          h("button", { type: "button", onclick: function () {
            api("/api/run/publish", { method: "POST", body: { h: S.h, key: key, to: to.value, min_severity: sev.value, statuses: nr.checked ? ["verified", "needs-runtime"] : ["verified"] } }).then(function (p) {
              var text = JSON.stringify(p, null, 2);
              show(p.records.length + " record(s) for " + p.destination + (p.held_back_over_limit ? ", " + p.held_back_over_limit + " held back" : ""), h("div", null,
                h("p", { class: "muted small", text: "Create these with the destination's MCP tools (the sh-publish skill does it): search each dedupe_query first, and comment instead of creating when it exists. Evidence and payloads never appear in a record." }),
                h("div", { class: "row" }, h("button", { type: "button", onclick: function () { saveFile("publish-plan-" + p.destination + ".json", text, "application/json"); } }, "Download"),
                  h("button", { type: "button", onclick: function () { navigator.clipboard.writeText(text).then(function () { toast("Copied"); }); } }, "Copy")),
                p.records.length ? h("ul", { class: "links" }, p.records.map(function (r) { return h("li", null, h("span", { class: "rel", text: r.severity }), r.title); })) : null,
                pre(text)));
            }).catch(function (e) { toast(e.message, true); });
          } }, "Build the plan")));
      }
      if (a.review) {
        var repo = h("input", { type: "text", value: a.repo || "", placeholder: "owner/repo", "aria-label": "Repository" });
        var pr = h("input", { type: "text", value: a.pr || "", placeholder: "PR number", "aria-label": "Pull request", size: "6" });
        var nostatus = h("input", { type: "checkbox" });
        function post(dry) {
          return api("/api/run/post", { method: "POST", body: { h: S.h, key: key, repo: repo.value.trim(), pr: pr.value.trim(), dry_run: dry, no_status: nostatus.checked } }).then(function (r) {
            show((dry ? "Dry run" : r.ok ? "Posted" : "Not posted (exit " + r.code + ")"), pre(r.output.join("\n")));
            if (!dry) toast(r.ok ? "Posted the review" : "Posting failed; the status was set to error", !r.ok);
          }).catch(function (e) { toast(e.message, true); });
        }
        card.appendChild(h("div", { class: "row" },
          h("b", { text: "Pull request" }), repo, pr, h("label", { class: "check" }, nostatus, h("span", { text: "leave the commit status" })),
          h("button", { type: "button", onclick: function () { post(true); } }, "Dry run"),
          ro ? null : h("button", { type: "button", class: "primary", onclick: function () {
            if (!window.confirm("Post this review to " + repo.value + "#" + pr.value + " (with gh, as you) and set its commit status?")) return;
            post(false);
          } }, "Post to GitHub")));
        if (!a.verdict) card.appendChild(h("p", { class: "muted small", text: "No review/verdict.json: the commit status would be set to error." }));
      }
      card.appendChild(out);
      box.appendChild(card);
    }).catch(function () { /* runs of other harnesses have no actions */ });
    return box;
  }

  // Local models: the Ollama server, its models (can each run Claude Code?), pulls.
  function ollamaCard() {
    var ro = S.ov.read_only;
    var url = h("input", { type: "text", placeholder: "http://localhost:11434", "aria-label": "Ollama server" });
    var out = h("div"), timer = null;
    var model = h("input", { type: "text", placeholder: "a model to pull, e.g. qwen3-coder", "aria-label": "Model to pull" });
    function load() {
      clearTimeout(timer);
      api("/api/ollama" + hq(url.value.trim() ? { url: url.value.trim() } : {})).then(function (o) {
        out.textContent = "";
        if (!url.value) url.placeholder = o.url;
        out.appendChild(o.reachable ? h("p", { class: "muted small" }, "Ollama at ", h("code", { text: o.url }), ": " + o.models.length + " model(s). Claude Code needs tool calling and a window of " + Math.round(o.min_context / 1024) + "k or more; run the server with OLLAMA_CONTEXT_LENGTH=65536.")
          : h("div", { class: "note warn", text: "Ollama is not answering at " + o.url + ": " + (o.error || "") + ". Start it with `ollama serve`." }));
        if (o.models.length) out.appendChild(h("div", { class: "tablewrap" }, h("table", null,
          h("thead", null, h("tr", null, ["Model", "Size", "Parameters", "Tool calling", "Max context", "Claude Code"].map(function (x) { return h("th", { text: x }); }))),
          h("tbody", null, o.models.map(function (x) {
            var fit = x.tools && (!x.max_context || x.max_context >= o.min_context);
            return h("tr", null, h("td", null, h("code", { text: x.name })), h("td", { class: "muted", text: x.size ? bytes(x.size) : "" }), h("td", { text: x.parameters || "" }),
              h("td", { text: x.tools == null ? "?" : x.tools ? "yes" : "no" }), h("td", { text: x.max_context ? Math.round(x.max_context / 1024) + "k" : "?" }),
              h("td", null, h("span", { class: "pill" + (fit ? "" : " warn"), text: fit ? "suitable" : x.tools === false ? "no tools" : "small window" })));
          })))));
        var pulls = o.pulls || {}, running = false;
        Object.keys(pulls).forEach(function (k) {
          var p = pulls[k]; if (!p.done) running = true;
          var pct = p.total ? " " + Math.round(100 * (p.completed || 0) / p.total) + "%" : "";
          out.appendChild(h("div", { class: "note" + (p.status === "failed" ? " warn" : p.done ? " ok" : "") }, h("b", { text: k }), ": " + (p.error || p.status) + pct));
        });
        if (running) timer = setTimeout(load, 1000);
      }).catch(function (e) { out.textContent = ""; out.appendChild(empty(e.message)); });
    }
    S.cleanup.push(function () { clearTimeout(timer); });
    load();
    return h("div", { class: "card ollama" }, h("h3", { text: "Local models (Ollama)" }),
      h("p", { class: "muted small" }, "Run Claude Code on one: a profile with Runs on: Ollama, Launch's Runs on, or ", h("code", { text: "indra claude -m MODEL" }), " in your own terminal."),
      h("div", { class: "row" }, url, h("button", { type: "button", onclick: load }, "Refresh"),
        canEdit() || (S.ov.is_admin && !ro) ? h("button", { type: "button", title: "Agents set to run on Ollama use this server unless their profile names another", onclick: function () {
          api("/api/ollama/default", { method: "PUT", body: { url: url.value.trim() || null } }).then(function (r) {
            url.value = ""; url.placeholder = r.url; toast("Saved: agents use Ollama at " + r.url); load();
          }).catch(function (err) { toast(err.message, true); });
        } }, "Save as default") : null),
      out,
      ro ? null : h("form", { class: "row", onsubmit: function (e) {
        e.preventDefault(); if (!model.value.trim()) return;
        api("/api/ollama/pull", { method: "POST", body: { model: model.value.trim(), url: url.value.trim() || null } }).then(function () { toast("Pulling " + model.value.trim()); load(); }).catch(function (err) { toast(err.message, true); });
      } }, model, h("button", { type: "submit" }, "Pull")));
  }

  // ---- tools --------------------------------------------------------------------------
  // The review tooling and repository chores, the same as their CLI commands.
  function pageTools(m) {
    var ro = S.ov.read_only || !S.ov.is_admin;
    m.appendChild(header("Tools", "The review cache, local metrics, the skills and the site: the same operations as indra review, indra skills and indra dev, from here."));
    var body = h("div", { class: "stack toolspage" });
    m.appendChild(ollamaCard()); m.appendChild(body);
    function load() {
      body.textContent = "";
      api("/api/tools").then(function (t) {
        // Review cache
        var c = t.cache || {}, items = c.items || [];
        body.appendChild(h("div", { class: "card" }, h("h3", { text: "PR review cache" }),
          h("p", { class: "muted small" }, "Under ", h("code", { text: c.root || "?" }), ". Knowledge-base hash ", h("code", { text: c.current_kb_version || "?" }),
            t.skills ? " (from " + t.skills + ")" : " (no knowledge bases found: every lookup misses)", "."),
          items.length ? h("div", { class: "tablewrap" }, h("table", null, h("thead", null, h("tr", null, ["Repository", "PR", "Head", "Findings", "Cached", "Size", ""].map(function (x) { return h("th", { text: x }); }))),
            h("tbody", null, items.map(function (i) {
              return h("tr", null, h("td", { text: i.repo }), h("td", { text: String(i.pr == null ? "" : i.pr) }), h("td", null, h("code", { text: i.head_sha || "" })),
                h("td", { text: String(i.findings == null ? "" : i.findings) }), h("td", { class: "muted", text: (i.created_at || "").slice(0, 16).replace("T", " ") + (i.stale_kb ? "  (knowledge bases changed)" : "") }),
                h("td", { class: "muted", text: bytes(i.bytes || 0) }),
                h("td", null, ro ? null : h("button", { type: "button", class: "ghost", onclick: function () { cacheOp({ action: "clear", repo: i.repo }); } }, "Clear repo")));
            })))) : h("p", { class: "muted", text: "Nothing cached." }),
          ro ? null : h("div", { class: "row" },
            h("button", { type: "button", onclick: function () { cacheOp({ action: "prune", max_age_days: 7 }); } }, "Prune entries older than 7 days"),
            h("button", { type: "button", class: "danger", onclick: function () { if (window.confirm("Clear the whole PR review cache?")) cacheOp({ action: "clear" }); } }, "Clear all"))));
        // Metrics
        var mt = t.metrics || {}, tot = mt.totals || {}, by = mt.by_model || {};
        body.appendChild(h("div", { class: "card" }, h("h3", { text: "Local run metrics" }),
          h("p", { class: "muted small" }, "Local only, in ", h("code", { text: (t.metrics_path || {}).dir || "?" }), ". Nothing is sent anywhere."),
          h("div", { class: "stats" }, [["runs", tot.runs], ["incomplete", tot.incomplete], ["agents", tot.agents], ["cost", tot.cost_usd != null ? "$" + tot.cost_usd : "-"], ["time", tot.duration_ms ? Math.round(tot.duration_ms / 1000) + "s" : "-"]].map(function (x) {
            return h("div", { class: "stat" }, h("b", { text: String(x[1] == null ? 0 : x[1]) }), h("span", { text: x[0] }));
          })),
          Object.keys(by).length ? h("div", { class: "tablewrap" }, h("table", null, h("thead", null, h("tr", null, ["Model", "Events", "Tokens in", "Tokens out", "Time"].map(function (x) { return h("th", { text: x }); }))),
            h("tbody", null, Object.keys(by).map(function (k) { var b = by[k]; return h("tr", null, h("td", { text: k }), h("td", { text: String(b.events) }), h("td", { text: String(b.tokens_in) }), h("td", { text: String(b.tokens_out) }), h("td", { text: Math.round(b.ms / 1000) + "s" })); })))) : null,
          (mt.runs || []).length ? h("details", { keep: "metrics-runs" }, h("summary", { text: "Runs (" + mt.runs.length + ")" }), h("div", { class: "tablewrap" }, h("table", null,
            h("thead", null, h("tr", null, ["Run", "Repository", "PR", "Verdict", "Agents", "Reused", "Cost"].map(function (x) { return h("th", { text: x }); }))),
            h("tbody", null, mt.runs.slice().reverse().map(function (r) {
              return h("tr", null, h("td", null, h("code", { text: r.run_id })), h("td", { text: r.repo || "" }), h("td", { text: r.pr || "" }), h("td", { text: (r.verdict || (r.complete ? "" : "incomplete")) }),
                h("td", { text: String(r.agents == null ? "" : r.agents) }), h("td", { text: String(r.reused_files == null ? "" : r.reused_files) }), h("td", { text: r.cost_usd != null ? "$" + r.cost_usd : "" }));
            }))))) : h("p", { class: "muted", text: "No runs recorded yet." }),
          ro ? null : h("div", { class: "row" },
            h("button", { type: "button", onclick: function () { api("/api/tools/metrics/purge", { method: "POST", body: { older_than_days: 30 } }).then(function (r) { toast("Dropped " + r.dropped + " record(s)"); load(); }).catch(function (e) { toast(e.message, true); }); } }, "Drop records older than 30 days"),
            h("button", { type: "button", class: "danger", onclick: function () { if (window.confirm("Delete all run metrics?")) api("/api/tools/metrics/purge", { method: "POST", body: {} }).then(function () { toast("Deleted"); load(); }).catch(function (e) { toast(e.message, true); }); } }, "Delete all"))));
        // Skills
        var hs = harness();
        var to = h("select", { "aria-label": "Install for" }, [["agents", "Codex, opencode and agentskills.io agents (.agents/skills)"], ["claude", "Claude Code (.claude/skills)"], ["opencode", "opencode (.config/opencode/skills)"]].map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
        var scope = h("select", { "aria-label": "Where" }, h("option", { value: "user", text: "for me (home folder)" }), (hs ? hs.targets : []).map(function (tg, i) { return h("option", { value: String(i), text: "in " + tg }); }));
        var mirror = t.mirror;
        body.appendChild(h("div", { class: "card" }, h("h3", { text: "Skills" }),
          t.repo ? h("p", { class: "muted small" }, "Repository ", h("code", { text: t.repo }), ". ", mirror ? (mirror.problems.length ? h("b", { class: "warn", text: "The skills/ mirror is stale (" + mirror.problems.length + " difference(s))." }) : "The skills/ mirror is current (" + mirror.files + " files).") : "")
            : h("p", { class: "muted", text: "This console does not see the indra repository, so it cannot sync or install its skills. The Marketplace page installs skills from any source." }),
          mirror && mirror.problems.length ? h("pre", { class: "view small", text: mirror.problems.join("\n") }) : null,
          t.repo && !ro ? h("div", { class: "row" },
            h("button", { type: "button", onclick: function () { api("/api/tools/skills/sync", { method: "POST" }).then(function (r) { toast("Synced " + r.files + " files"); load(); }).catch(function (e) { toast(e.message, true); }); } }, "Sync the mirror"),
            to, scope,
            h("button", { type: "button", class: "primary", onclick: function () {
              var body2 = { to: to.value, h: S.h }; if (scope.value !== "user") { body2.scope = "workspace"; body2.target = Number(scope.value); }
              api("/api/tools/skills/install", { method: "POST", body: body2 }).then(function (r) { toast("Installed " + r.skills + " skills in " + r.dir); }).catch(function (e) { toast(e.message, true); });
            } }, "Install the skills")) : null));
        // Repository chores
        if (t.repo) {
          var res = h("div");
          body.appendChild(h("div", { class: "card" }, h("h3", { text: "Repository" }),
            h("p", { class: "muted small", text: "Maintainer chores: build the website (site/ plus the docs listed in site/pages.toml) into _site, and look for em and en dashes." }),
            h("div", { class: "row" },
              ro ? null : h("button", { type: "button", onclick: function (e) {
                e.target.disabled = true;
                api("/api/tools/site", { method: "POST" }).then(function (r) { res.textContent = ""; res.appendChild(h("div", { class: "note ok", text: "Built " + r.pages + " docs pages into " + r.out + "; every link checks out." })); })
                  .catch(function (err) { res.textContent = ""; res.appendChild(h("pre", { class: "view small", text: err.message })); }).then(function () { e.target.disabled = false; });
              } }, "Build the site"),
              h("button", { type: "button", onclick: function () {
                api("/api/tools/dashes").then(function (r) {
                  res.textContent = "";
                  res.appendChild(r.found.length ? h("pre", { class: "view small", text: r.found.map(function (d) { return d.file + ":" + d.line + ": " + d.text; }).join("\n") }) : h("div", { class: "note ok", text: "No em or en dashes." }));
                }).catch(function (e) { toast(e.message, true); });
              } }, "Check for dashes")), res));
        }
      }).catch(function (e) { body.appendChild(empty(e.message)); });
    }
    function cacheOp(b) { api("/api/tools/cache", { method: "POST", body: b }).then(function (r) { toast(r.removed != null ? "Removed " + r.removed + " entr(ies)" : "Cleared"); load(); }).catch(function (e) { toast(e.message, true); }); }
    load();
  }

  function pageRun(m, key) {
    var top = h("div"), body = h("div"), viewer = h("div");
    m.appendChild(top); m.appendChild(runActions(key)); m.appendChild(runThreats(key)); m.appendChild(runFindings(key)); m.appendChild(body);
    var openRel = null, lastMod = 0;
    function showFile(rel) {
      openRel = rel;
      Array.prototype.forEach.call(body.querySelectorAll(".files a"), function (a) { a.classList.toggle("on", a.dataset.rel === rel); });
      api("/api/run/file" + hq({ key: key, rel: rel })).then(function (f) {
        viewer.textContent = "";
        viewer.appendChild(h("div", { class: "row" }, h("b", { text: f.rel }), f.truncated ? h("span", { class: "pill warn", text: "showing the end of a large file" }) : null, h("span", { class: "muted", text: ago(f.modified) })));
        if (/\.(md|markdown)$/i.test(f.rel) && !f.truncated && f.text) {
          viewer.appendChild(h("div", { class: "view md-view" }, mdOrSource(f.text)));
          return;
        }
        var pre = h("pre", { class: "view", text: f.text || "(empty)" });
        viewer.appendChild(pre);
        if (f.truncated) pre.scrollTop = pre.scrollHeight;
      }).catch(function (e) { toast(e.message, true); });
    }
    function load() {
      api("/api/run" + hq({ key: key })).then(function (r) {
        top.textContent = "";
        top.appendChild(header(r.name, r.path, [r.active ? h("span", { class: "pill run live", text: "live" }) : h("span", { class: "pill", text: "idle " + ago(r.modified) })]));
        top.appendChild(stagePipe(r.stages));
        if (r.modified === lastMod && body.childNodes.length) return;
        lastMod = r.modified;
        body.textContent = "";
        var c = r.counts;
        body.appendChild(h("div", { class: "grid" },
          h("div", { class: "card" }, h("h3", { text: "Results by agent / class" + (c.source ? " (" + c.source + ")" : "") }), bars(c.by_group)),
          h("div", { class: "card" }, h("h3", { text: "By status" }), bars(c.by_status), h("h3", { text: "By severity" }), bars(c.by_severity))));
        var files = h("div", { class: "files" }, (r.files || []).map(function (f) {
          return h("a", { href: "#", "data-rel": f.rel, onclick: function (e) { e.preventDefault(); showFile(f.rel); } }, f.rel, h("span", { text: bytes(f.size) + ", " + ago(f.modified) }));
        }));
        body.appendChild(h("h2", { text: "Files" }));
        body.appendChild(h("div", { class: "split" }, viewer, h("div", { class: "card" }, files)));
        if (openRel) showFile(openRel);
        else if (r.files && r.files.length) {
          var pick = r.files.slice().sort(function (a, b) { return b.modified - a.modified; })[0];
          showFile(pick.rel);
        }
      }).catch(function (e) { top.textContent = ""; top.appendChild(empty(e.message)); });
    }
    load(); every(3000, load);
  }

  // Nodes, fetched once per page that needs them.
  function loadNodes(refresh) { return api("/api/nodes" + hq(refresh ? { refresh: 1 } : {})); }
  function runnersOn(node, ov) {
    if (node.name === "local") return ov.runners.filter(function (r) { return r.available; }).map(function (r) { return r.name; });
    return node.info ? node.info.runners.filter(function (r) { return r.available; }).map(function (r) { return r.name; }) : [];
  }

  function pageLaunch(m) {
    var ov = S.ov, hs = harness();
    m.appendChild(header("Run an agent", "Choose where it works and what it should do; you watch it live and can answer or steer it."));
    m.appendChild(launchTabs("one"));
    if (ov.read_only) { m.appendChild(h("div", { class: "note warn", text: "indra is running read-only, so launching is disabled." })); return; }
    if (!hs) { m.appendChild(empty("No project yet: start indra in your project's folder (cd my-project, then indra serve), or pass --target <folder>.")); return; }
    var holder = h("div", { class: "muted", text: "Checking machines..." });
    m.appendChild(holder);
    loadNodes(false).then(function (nodes) {
      holder.remove();
      var node = h("select", { "aria-label": "Machine" }, nodes.map(function (n) {
        return h("option", { value: n.name, text: n.name + (n.kind === "local" ? "  (this machine)" : n.info ? "" : "  (offline)") });
      }));
      var runner = h("select");
      var target = h("select");
      var presets = ov.presets.filter(function (p) { return !p.harness || p.harness === hs.name; });
      var preset = h("select", { "aria-label": "Start from" }, h("option", { value: "", text: "Write your own" }), presets.map(function (p, i) { return h("option", { value: String(i), text: p.name }); }));
      var prompt = h("textarea", { placeholder: "What should the agent do?", "aria-label": "Prompt" });
      // A draft survives leaving the page (and switching setups) until it runs.
      try { prompt.value = sessionStorage.getItem("indra.draft.launch") || ""; } catch (e) { /* storage off */ }
      prompt.addEventListener("input", function () { try { sessionStorage.setItem("indra.draft.launch", prompt.value); } catch (e) { /* storage off */ } });
      var profiles = ov.profiles || [];
      var profile = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: "None (runner defaults)" }),
        profiles.map(function (p) { return h("option", { value: p.name, text: p.name + (p.description ? "  -  " + p.description : "") }); }));
      var model = h("input", { type: "text", list: "models", "aria-label": "Model", placeholder: "default" });
      var mode = h("select", { "aria-label": "Permission mode" }, h("option", { value: "", text: "default" }),
        MODES.map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
      var effort = h("select", { "aria-label": "Effort" }, h("option", { value: "", text: "default" }),
        ["low", "medium", "high", "xhigh", "max"].map(function (x) { return h("option", { value: x, text: x }); }));
      var mcp = h("select", { "aria-label": "MCP servers" }, [["", "default"], ["all", "all, as Claude Code is set up"], ["none", "none: a much smaller context"]].map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
      var where = h("select", { "aria-label": "Runs on" }, [["", "as configured"], ["anthropic", "Anthropic (the runner's own setup)"], ["ollama", "Ollama: the whole agent on a local model"],
        ["subagents", "Anthropic, with its subagents on Ollama"]].map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
      var subModel = h("input", { type: "text", list: "models", "aria-label": "Subagent model", placeholder: "an Ollama model, e.g. qwen3-coder" });
      var subLabel = h("label", { hidden: true }, "Subagents' Ollama model", subModel);
      var memory = h("input", { type: "checkbox" }), reflect = h("input", { type: "checkbox" });
      var memNote = h("div", { class: "muted small" });
      function prof() { return profiles.find(function (p) { return p.name === profile.value; }); }
      function fillProfile() {
        var p = prof() || {};
        memory.checked = !!p.memory; reflect.checked = !!p.reflect;
        if (p.runner && Array.prototype.some.call(runner.options, function (o) { return o.value === p.runner; })) runner.value = p.runner;
        model.placeholder = p.model || "default"; mode.options[0].text = p.permission_mode ? "profile: " + p.permission_mode : "default";
        where.options[0].text = p.provider === "ollama" ? "profile: Ollama" : p.subagent_provider === "ollama" ? "profile: subagents on Ollama" : "as configured";
        effort.options[0].text = p.effort ? "profile: " + p.effort : "default";
        mcp.options[0].text = p.mcp === "none" ? "profile: none" : "default (all, as Claude Code is set up)";
        update();
      }
      profile.addEventListener("change", fillProfile);
      [model, mode, effort, mcp, where, subModel].forEach(function (el) { el.addEventListener("input", update); el.addEventListener("change", update); });
      var preview = h("div", { class: "cmd" });
      var go = h("button", { class: "primary", type: "submit" }, "Run");
      function cur() { return nodes.find(function (n) { return n.name === node.value; }) || nodes[0]; }
      function fillNode() {
        var n = cur(), names = runnersOn(n, ov), keep = runner.value;
        runner.textContent = ""; target.textContent = "";
        names.forEach(function (r) { runner.appendChild(h("option", { value: r, text: r })); });
        if (names.indexOf(keep) >= 0) runner.value = keep;
        n.targets.forEach(function (t, i) { target.appendChild(h("option", { value: String(i), text: t })); });
        update();
      }
      function update() {
        var n = cur(), t = n.targets[Number(target.value)] || "", p = prompt.value.split("{target}").join(t).split("{node}").join(n.name);
        if (n.kind === "local") {
          var r = ov.runners.find(function (x) { return x.name === runner.value; });
          var pr = prof() || {}, extra = [];
          if (r && r.format === "claude-stream-json") {
            [["--model", model.value || pr.model], ["--permission-mode", mode.value || pr.permission_mode], ["--effort", effort.value || pr.effort]]
              .forEach(function (f) { if (f[1]) extra.push(f[0], f[1]); });
            if ((mcp.value || pr.mcp) === "none") extra.push("--strict-mcp-config", "--mcp-config", '{"mcpServers":{}}');
          }
          var viaStdin = r && r.format === "claude-stream-json" && r.args.indexOf("{prompt}") >= 0;
          preview.textContent = r ? [r.command].concat(r.args.filter(function (a) { return !(viaStdin && a === "{prompt}"); })
            .map(function (a) { return a.split("{target}").join(t).split("{prompt}").join(p); })).concat(viaStdin ? ["--input-format", "stream-json"] : []).concat(extra)
            .map(function (a) { return /[\s"'$`\;&|<>]/.test(a) ? JSON.stringify(a) : a; }).join(" ") : "";
          if (viaStdin) preview.textContent += "\n(the prompt goes over stdin, so you can approve its tool requests and change its mode and model while it runs)";
          var on = where.value || (pr.provider === "ollama" ? "ollama" : pr.subagent_provider === "ollama" ? "subagents" : "");
          if (on === "ollama") preview.textContent += "\n(on Ollama at " + (pr.ollama_url || "http://localhost:11434") + ": every model it asks for, subagents included, is " + (model.value || pr.model || "the model you pick") + ")";
          if (on === "subagents") preview.textContent += "\n(the main agent stays on Anthropic; requests for " + (subModel.value || pr.subagent_model || "the subagent model") + " go to Ollama through indra's model router)";
        } else {
          preview.textContent = (n.ssh && n.transport === "ssh" ? "ssh " + n.ssh + " -- " : "(over its link) ") + "indra node exec   (job: " + JSON.stringify({ runner: runner.value, target: t, prompt: p }) + ")";
        }
        var why = !runner.value ? (n.info ? "No agent (Claude Code, Codex, Gemini) is installed on this machine." : "This machine is offline" + (n.error ? ": " + n.error : ".")) : !n.targets.length ? "This machine has no folder for agents yet." + (n.kind === "local" ? " Start indra in your project folder, or pass --target." : " On it, run `indra node connect --target <folder>`.") : "";
        note.textContent = why; note.hidden = !why;
        subLabel.hidden = where.value !== "subagents";
        go.disabled = !prompt.value.trim() || !runner.value || !n.targets.length;
        preflight();
      }
      // Before launching on Ollama: is the model there, and will it work?
      var pfNote = h("div", { class: "note warn", hidden: true }), pfKey = "", pfTimer = null;
      function preflight() {
        if (!pfNote) return;
        var pr = prof() || {};
        var on = where.value || (pr.provider === "ollama" ? "ollama" : pr.subagent_provider === "ollama" ? "subagents" : "");
        var m = on === "ollama" ? (model.value.trim() || pr.model) : on === "subagents" ? (subModel.value.trim() || pr.subagent_model) : "";
        var key = on && m ? m + "@" + (pr.ollama_url || "") : "";
        if (key === pfKey) return;
        pfKey = key; clearTimeout(pfTimer);
        if (!key || cur().kind !== "local") { pfNote.hidden = true; return; }
        pfTimer = setTimeout(function () {
          api("/api/ollama/check" + hq(Object.assign({ model: m }, pr.ollama_url ? { url: pr.ollama_url } : {}))).then(function (c) {
            if (key !== pfKey) return;
            var msgs = (c.ok ? [] : [c.error]).concat(c.warnings || []);
            pfNote.textContent = msgs.join(" ");
            pfNote.hidden = !msgs.length;
          }).catch(function () { pfNote.hidden = true; });
        }, 400);
      }
      var note = h("div", { class: "note warn", hidden: true });
      preset.addEventListener("change", function () {
        var p = presets[Number(preset.value)];
        if (p) { prompt.value = p.prompt; if (p.runner) { runner.value = p.runner; } if (p.profile) { profile.value = p.profile; fillProfile(); } }
        update();
      });
      node.addEventListener("change", fillNode);
      [runner, target, prompt].forEach(function (el) { el.addEventListener("input", update); el.addEventListener("change", update); });
      var form = h("form", { class: "form", "data-nodirty": "", onsubmit: function (e) {
        e.preventDefault(); go.disabled = true;
        var o = {};
        if (model.value.trim()) o.model = model.value.trim();
        if (mode.value) o.permission_mode = mode.value;
        if (effort.value) o.effort = effort.value;
        if (mcp.value) o.mcp = mcp.value;
        if (where.value === "ollama" || where.value === "anthropic") o.provider = where.value;
        if (where.value === "subagents") { o.subagent_provider = "ollama"; if (subModel.value.trim()) o.env = { CLAUDE_CODE_SUBAGENT_MODEL: subModel.value.trim() }; }
        api("/api/procs", { method: "POST", body: { h: hs.name, node: node.value, runner: runner.value, prompt: prompt.value, target: Number(target.value), profile: profile.value || null, opts: o, memory: memory.checked, reflect: reflect.checked } })
          .then(function (r) { try { sessionStorage.removeItem("indra.draft.launch"); } catch (e) { /* storage off */ } location.hash = "#/sessions/" + r.id; })
          .catch(function (err) { toast(err.message, true); go.disabled = false; });
      } },
        h("label", null, "What should it do?", h("small", { text: "Plain words are fine. {target} becomes the folder, {node} the machine." }), prompt),
        h("div", { class: "fields" }, h("label", null, "Start from", preset), h("label", null, "Machine", node), h("label", null, "Agent", runner),
          h("label", { class: "wide" }, h("span", null, "Folder ", S.ov.is_admin ? h("a", { href: "#/nodes", class: "small", title: "Add or change the folders agents may work in, on any machine", text: "(change folders)" }) : null), target)),
        note,
        h("details", { class: "card" }, h("summary", { text: "More options: profile, model, permissions, memory" }),
          h("div", { class: "fields" }, h("label", null, "Profile", profile), h("label", null, "Model", model), h("label", null, "Permission mode", mode), h("label", null, "Effort", effort)),
          h("div", { class: "fields" }, h("label", null, "MCP servers", mcp, h("small", { text: "Each server's tools fill the context before the agent does anything." })), h("label", null, "Runs on", where), subLabel),
          pfNote,
          modelList(),
          h("div", { class: "knowctl" },
            h("label", { class: "check" }, memory, h("span", null, h("b", { text: "Give it relevant memory" }), h("small", { text: " - the notes from this harness's vaults that best match the prompt are added after it" }))),
            h("div", { class: "row" }, h("button", { type: "button", class: "ghost", onclick: function () {
              var p = prompt.value.trim(); if (!p) { memNote.textContent = "Write the prompt first."; return; }
              memNote.textContent = "Recalling...";
              api("/api/knowledge/recall" + hq({ q: p })).then(function (r) {
                memNote.textContent = r.notes.length ? "It would get: " + r.notes.map(function (n) { return n.title; }).join("; ") : "Nothing relevant in memory for this prompt.";
              }).catch(function (e) { memNote.textContent = e.message; });
            } }, "Preview what it would get"), memNote),
            h("label", { class: "check" }, reflect, h("span", null, h("b", { text: "Ask for learnings at the end" }), h("small", { text: " - before it finishes, a Claude Code agent proposes what it learned; review them under Knowledge > Learnings" })))),
          h("label", null, "Command", h("small", { text: "Executed directly, never through a shell. On another machine the job travels as JSON over its link." }), preview),
          h("div", { class: "note", text: "The agent runs with that machine's user permissions and its agent CLI's own permission settings. Another machine runs only agents from its own configuration." })),
        h("div", { class: "row" }, go, canEdit() ? h("button", { type: "button", onclick: function () {
          ask("Save as a preset", [{ name: "name", label: "Name", hint: "It appears under Start from, for everyone using this console." }], "Save", function (v) {
            var p = { name: v.name.trim(), prompt: prompt.value, harness: hs.name };
            if (runner.value) p.runner = runner.value;
            if (profile.value) p.profile = profile.value;
            return addToSection("preset", p).then(function () { toast("Saved preset " + p.name); });
          });
        } }, "Save as preset") : null, h("a", { href: "#/workflows", onclick: function () { setTimeout(function () { var f = $("fanout"); if (f) f.scrollIntoView(); }, 600); }, text: "Or run on many folders at once" })));
      m.appendChild(form);
      fillNode();
    }).catch(function (e) { holder.textContent = e.message; });
  }

  // ---- nodes --------------------------------------------------------------------
  function meter(used, max) {
    var fill = h("div", { class: "fill" }); fill.style.width = Math.min(100, 100 * used / Math.max(1, max)) + "%";
    return h("div", { class: "bar" }, h("span", { text: "running now" }), h("div", { class: "track" }, fill), h("b", { text: used + "/" + max }));
  }
  var INSTALL_SH = "https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.sh";
  var INSTALL_PS = "https://raw.githubusercontent.com/dmdhrumilmistry/indra/main/install.ps1";
  // A command in a box, with a Copy button.
  function cmdBox(text) {
    var pre = h("pre", { class: "view", text: text });
    return h("div", { class: "cmdbox" }, pre, h("button", { type: "button", onclick: function () {
      var t = pre.textContent;
      (navigator.clipboard ? navigator.clipboard.writeText(t) : Promise.reject()).then(function () { toast("Copied"); })
        .catch(function () { var r = document.createRange(); r.selectNodeContents(pre); var sel = getSelection(); sel.removeAllRanges(); sel.addRange(r); toast("Selected: press Ctrl+C"); });
    } }, "Copy"));
  }
  function canJoin() { return !!(S.ov && S.ov.settings && S.ov.settings.join); }

  function pageNodes(m) {
    var refresh = h("button", { type: "button" }, "Check again");
    m.appendChild(header("Machines", "Computers your agents run on. Add one with a single command: no SSH, no open ports, and it reconnects by itself.",
      [canJoin() ? h("button", { type: "button", class: "primary", onclick: function () { addMachine(); } }, "Add a machine") : null,
        canJoin() ? h("button", { type: "button", title: "One key for many machines: for Jamf, Intune or any script", onclick: function () { manyMachines(); } }, "Add many machines") : null,
        canEdit() ? h("a", { class: "btn", href: "#/settings/nodes", text: "Advanced (SSH)" }) : null, refresh]));
    var addBox = h("div"); m.appendChild(addBox);
    if (!canJoin() && S.ov.is_admin && S.ov.settings && S.ov.settings.join_locked)
      m.appendChild(h("div", { class: "note warn", text: "Machines cannot be added here: " + S.ov.settings.join_locked + "." }));
    var stopPoll = null;
    S.cleanup.push(function () { if (stopPoll) stopPoll(); });
    // Many machines at once (MDM): an enrollment key, scripts that use it,
    // and the keys made so far.
    function manyMachines() {
      if (stopPoll) stopPoll();
      addBox.textContent = "";
      var label = h("input", { type: "text", placeholder: "e.g. engineering laptops", "aria-label": "Label" });
      var days = h("select", { "aria-label": "Valid for" }, [[7, "7 days"], [30, "30 days"], [90, "90 days"], [365, "a year"]].map(function (d) { return h("option", { value: String(d[0]), text: d[1] }); }));
      days.value = "30";
      var max = h("input", { type: "number", min: "1", placeholder: "no limit", "aria-label": "At most this many machines" });
      var made = h("div"), list = h("div");
      var card = h("div", { class: "card form" },
        h("div", { class: "row" }, h("h2", { text: "Add many machines" }), h("button", { type: "button", class: "ghost", "aria-label": "Close", onclick: function () { addBox.textContent = ""; } }, "x")),
        h("p", { class: "muted" }, "Make an enrollment key, then roll indra out with your device management (Jamf, Microsoft Intune, Kandji, ...) or any script: each machine installs indra and joins with the key. Unlike a one-time code, a key works for many machines until it expires, reaches its limit or you revoke it. See ",
          h("a", { href: "https://github.com/dmdhrumilmistry/indra/blob/main/docs/guides/mdm.md", target: "_blank", rel: "noopener", text: "the MDM guide" }), "."),
        h("form", { class: "fields", onsubmit: function (e) {
          e.preventDefault();
          var body = { label: label.value.trim(), days: Number(days.value) };
          if (max.value) body.max_uses = Number(max.value);
          api("/api/nodes/keys", { method: "POST", body: body }).then(function (k) { showKey(k); loadKeys(); })
            .catch(function (err) { made.textContent = ""; made.appendChild(h("div", { class: "note bad", role: "alert", text: err.message })); });
        } }, h("label", null, "Label", label), h("label", null, "Valid for", days), h("label", null, "At most (machines)", max),
          h("div", { class: "row" }, h("button", { type: "submit", class: "primary" }, "Make a key"))),
        made, h("h3", { text: "Keys" }), list);
      addBox.appendChild(card);
      function showKey(k) {
        var J = k.hub + " " + k.key;
        var scripts = {
          jamf: "#!/bin/sh\n# Jamf Pro: Settings > Scripts, then a policy that runs it once per computer.\n# It installs indra for the signed-in user and joins this hub.\nuser=$(stat -f%Su /dev/console)\n{ [ -z \"$user\" ] || [ \"$user\" = root ]; } && exit 0\nlaunchctl asuser \"$(id -u \"$user\")\" sudo -u \"$user\" -H sh -c \"cd ~ && curl -fsSL " + INSTALL_SH + " | INDRA_JOIN='" + J + "' sh\"\n",
          intune: "# Microsoft Intune: Devices > Scripts and remediations > Platform scripts > Add (Windows 10 and later).\n# Run this script using the logged on credentials: Yes. Run in 64-bit PowerShell: Yes.\nSet-Location $HOME\n$env:INDRA_JOIN = \"" + J + "\"\nirm " + INSTALL_PS + " | iex\n",
          linux: "#!/bin/sh\n# Any Linux fleet tool (Ansible, Salt, a login script): as the user who will run agents.\ncd ~ && curl -fsSL " + INSTALL_SH + " | INDRA_JOIN='" + J + "' sh\n",
          file: "# managed.toml in the organization's folder (deploy it with your MDM):\n#   macOS   /Library/Application Support/indra/managed.toml\n#   Linux   /etc/indra/managed.toml\n#   Windows C:\\ProgramData\\indra\\managed.toml\n# Then `indra join` with no arguments joins this hub.\n[join]\nhub = \"" + k.hub + "\"\nkey = \"" + k.key + "\"\n"
        };
        var tab = "jamf", tabs = h("div", { class: "tabs", role: "tablist" }), box = h("div");
        function draw() {
          tabs.textContent = ""; box.textContent = "";
          [["jamf", "Jamf (macOS)"], ["intune", "Intune (Windows)"], ["linux", "Linux"], ["file", "Settings file"]].forEach(function (t) {
            tabs.appendChild(h("button", { type: "button", role: "tab", class: t[0] === tab ? "on" : "", "aria-selected": String(t[0] === tab), onclick: function () { tab = t[0]; draw(); } }, t[1]));
          });
          box.appendChild(cmdBox(scripts[tab]));
        }
        draw();
        made.textContent = "";
        made.appendChild(h("div", { class: "card joined" },
          h("h3", { text: "Key made: " + k.label }),
          k.reachable ? null : h("div", { class: "note warn", text: "This console listens on this computer only, so machines cannot reach it yet: restart it with indra serve --bind 0.0.0.0:7878." }),
          h("p", null, h("b", { text: "Copy it now: " }), "indra keeps only a fingerprint of it and cannot show it again."),
          cmdBox(k.key), h("p", { class: "muted small", text: "Ready to use, with this hub and key filled in:" }), tabs, box));
      }
      function loadKeys() {
        api("/api/nodes/keys").then(function (keys) {
          list.textContent = "";
          if (!keys.length) { list.appendChild(h("p", { class: "muted", text: "No keys yet." })); return; }
          list.appendChild(h("div", { class: "tablewrap" }, h("table", null,
            h("thead", null, h("tr", null, ["Key", "State", "Machines", "Expires", ""].map(function (x) { return h("th", { text: x }); }))),
            h("tbody", null, keys.map(function (k) {
              return h("tr", null, h("td", null, h("b", { text: k.label }), h("div", { class: "muted small", text: "made " + ago(k.created) })),
                h("td", null, h("span", { class: "pill " + (k.state === "active" ? "ok" : "warn"), text: k.state })),
                h("td", { title: (k.machines || []).join(", "), text: k.uses + (k.max_uses ? " of " + k.max_uses : "") }),
                h("td", { class: "muted", text: new Date(k.expires * 1000).toLocaleDateString() }),
                h("td", null, k.state === "active" ? h("button", { type: "button", class: "ghost", onclick: function () {
                  if (!confirm("Revoke " + k.label + "? Machines that joined with it stay; no new ones can.")) return;
                  api("/api/nodes/keys/" + encodeURIComponent(k.id), { method: "DELETE" }).then(loadKeys).catch(function (e) { toast(e.message, true); });
                } }, "Revoke") : null));
            })))));
        }).catch(function (e) { list.textContent = ""; list.appendChild(h("div", { class: "note bad", text: e.message })); });
      }
      loadKeys();
      label.focus();
    }
    // One click: a code, the command to run there, and then what joined.
    function addMachine() {
      if (stopPoll) stopPoll();
      addBox.textContent = "";
      var card = h("div", { class: "card form", "data-nodirty": "" }, h("h2", { text: "Add a machine" }), h("p", { class: "muted", text: "Getting a code..." }));
      addBox.appendChild(card);
      api("/api/nodes/enrol", { method: "POST", body: {} }).then(function (r) {
        card.textContent = "";
        card.appendChild(h("div", { class: "row" }, h("h2", { text: "Add a machine" }),
          h("button", { type: "button", class: "ghost", "aria-label": "Close", onclick: function () { if (stopPoll) stopPoll(); addBox.textContent = ""; } }, "x")));
        if (!r.reachable) card.appendChild(h("div", { class: "note warn" }, h("b", { text: "Other machines cannot reach this console yet. " }),
          "It listens on this computer only. Restart it with ", h("code", { text: "indra serve --bind 0.0.0.0:7878" }), " and open it by this computer's network address, then add the machine."));
        card.appendChild(h("p", null, "Run this on the machine you want to add. It installs indra if needed, joins with code ",
          h("b", { text: r.code }), " (works once, for an hour), and keeps running in the background. Its name, system and agents are detected for you."));
        var name = h("input", { type: "text", placeholder: "its computer name", "aria-label": "Name" });
        var folder = h("input", { type: "text", placeholder: "the folder the command runs in", "aria-label": "Folder" });
        var tab = store("indra.jointab") || (/Win/.test(navigator.platform) ? "win" : "unix");
        var tabs = h("div", { class: "tabs", role: "tablist" }), boxHolder = h("div");
        var kinds = [["unix", "Linux or macOS"], ["win", "Windows (PowerShell)"], ["have", "indra already installed"]];
        function q(v) { return '"' + v.replace(/"/g, "") + '"'; }
        function commands() {
          var n = name.value.trim(), f = folder.value.trim(), join = r.hub + " " + r.code;
          var env = 'INDRA_JOIN="' + join + '"' + (n ? " INDRA_JOIN_NAME=" + q(n) : "") + (f ? " INDRA_JOIN_TARGET=" + q(f) : "");
          var penv = '$env:INDRA_JOIN="' + join + '"; ' + (n ? "$env:INDRA_JOIN_NAME=" + q(n) + "; " : "") + (f ? "$env:INDRA_JOIN_TARGET=" + q(f) + "; " : "");
          return {
            unix: "curl -fsSL " + INSTALL_SH + " | " + env + " sh",
            win: penv + "irm " + INSTALL_PS + " | iex",
            have: "indra join " + join + (n ? " --name " + q(n) : "") + (f ? " --target " + q(f) : "")
          };
        }
        function draw() {
          tabs.textContent = ""; boxHolder.textContent = "";
          kinds.forEach(function (k) {
            tabs.appendChild(h("button", { type: "button", role: "tab", class: k[0] === tab ? "on" : "", "aria-selected": String(k[0] === tab),
              onclick: function () { tab = k[0]; store("indra.jointab", tab); draw(); } }, k[1]));
          });
          boxHolder.appendChild(cmdBox(commands()[tab]));
          if (tab === "have") boxHolder.appendChild(h("p", { class: "muted small", text: "Needs indra 0.8.0 or newer there (`indra update`)." }));
        }
        name.addEventListener("input", draw); folder.addEventListener("input", draw);
        draw();
        card.appendChild(tabs); card.appendChild(boxHolder);
        card.appendChild(h("details", null, h("summary", { text: "Options: name and folder" }),
          h("label", null, "Name", h("small", { text: "How it is listed here. Default: the machine's own name." }), name),
          h("label", null, "Folder for agents", h("small", { text: "Where agents may work on that machine, e.g. D:\\work or ~/code. Default: the folder the command runs in." }), folder)));
        var status = h("div", { class: "note", role: "status" }, "Waiting for the machine to join...");
        card.appendChild(status);
        var timer = setInterval(poll, 2000), shown = false;
        stopPoll = function () { clearInterval(timer); stopPoll = null; };
        function poll() {
          api("/api/nodes/enrol/" + encodeURIComponent(r.code)).then(function (st) {
            if (!st.joined) return;
            if (!st.info && !shown) { status.textContent = st.joined + " joined; connecting..."; return; }
            if (!st.info) return;
            shown = true; stopPoll();
            var i = st.info;
            status.className = "card joined";
            status.textContent = "";
            status.appendChild(h("h3", { text: st.joined + " joined" }));
            status.appendChild(h("div", { class: "stats" }, [["computer", i.hostname], ["system", i.os + " " + i.arch], ["cpus", String(i.cpus)], ["indra", i.version]].map(function (x) {
              return h("div", { class: "stat" }, h("b", { class: "small", text: x[1] }), h("span", { text: x[0] }));
            })));
            status.appendChild(h("p", null, "Agents there: ", (i.runners || []).map(function (x) { return h("span", { class: "pill " + (x.available ? "ok" : ""), text: x.name + (x.available ? "" : " (not installed)") }); })));
            if ((st.targets || []).length) status.appendChild(h("p", { class: "muted small", text: "Agents work in: " + st.targets.join(", ") }));
            status.appendChild(h("div", { class: "row" }, h("a", { class: "btn primary", href: "#/launch", text: "Run an agent there" }),
              h("button", { type: "button", onclick: function () { addMachine(); } }, "Add another")));
            load(true);
          }).catch(function (e) { if (e.status === 404) { stopPoll(); status.className = "note bad"; status.textContent = "The code expired. Click Add a machine for a new one."; } });
        }
      }).catch(function (err) {
        card.textContent = "";
        card.appendChild(h("h2", { text: "Add a machine" }));
        card.appendChild(h("div", { class: "note bad", role: "alert", text: "Could not make a code: " + err.message }));
      });
    }
    var grid = h("div", { class: "grid" }), runsBox = h("div");
    m.appendChild(grid); m.appendChild(runsBox);
    function showRuns(n) {
      runsBox.textContent = "";
      runsBox.appendChild(h("h2", { text: "Results on " + n.name }));
      var box = h("div", { class: "muted", text: "Loading..." }); runsBox.appendChild(box);
      api("/api/nodes/runs" + hq({ node: n.name })).then(function (runs) {
        box.textContent = ""; box.className = "tablewrap";
        if (!runs.length) { box.appendChild(empty("No results in this machine's folders yet.")); return; }
        box.appendChild(h("table", null, h("thead", null, h("tr", null, ["Run", "Progress", "Results", "By agent", "Updated"].map(function (t) { return h("th", { text: t }); }))),
          h("tbody", null, runs.map(function (r) {
            var groups = Object.keys(r.counts.by_group).map(function (g) { return g + " " + r.counts.by_group[g]; }).join(", ");
            // A remote run opens as a copy fetched to the hub.
            var go = n.name === "local" ? function () { location.hash = "#/run/" + encodeURIComponent(r.key); } : function () {
              toast("Copying " + r.name + " from " + n.name + "...");
              api("/api/nodes/runs/fetch", { method: "POST", body: { h: S.h, node: n.name, name: r.name } }).then(function (f) {
                toast("Copied " + f.files + " file(s)" + (f.skipped.length ? ", " + f.skipped.length + " skipped (binary or large)" : "") + ". Changes stay in the copy.");
                location.hash = "#/run/" + encodeURIComponent(f.key);
              }).catch(function (e) { toast(e.message, true); });
            };
            return h("tr", { class: go ? "click" : "", onclick: go },
              h("td", null, h("b", { text: r.name }), " ", r.active ? h("span", { class: "pill run live", text: "live" }) : null, h("div", { class: "muted", text: r.target })),
              h("td", null, segs(r.stages)), h("td", { text: String(r.counts.total) }), h("td", { class: "desc", text: groups || "-" }), h("td", { class: "muted", text: ago(r.modified) }));
          }))));
      }).catch(function (e) { box.textContent = e.message; });
    }
    // Where agents work on a machine: a list to edit, and a folder browser
    // that walks the machine's folders (names only) over its link.
    function olderThan(a, b) {
      var x = String(a || "").split(/[.-]/).map(Number), y = String(b || "").split(/[.-]/).map(Number);
      for (var k = 0; k < 3; k++) { if ((x[k] || 0) !== (y[k] || 0)) return (x[k] || 0) < (y[k] || 0); }
      return false;
    }
    function folderEditor(n, box) {
      var list = n.targets.slice();
      var listBox = h("div", { class: "files" }), err = h("div", { class: "note bad", role: "alert", hidden: true });
      var typed = h("input", { type: "text", placeholder: n.info && n.info.os === "windows" ? "D:\\code\\my-app" : "/home/me/code/my-app", "aria-label": "Folder path" });
      var browser = h("div");
      function drawList() {
        listBox.textContent = "";
        if (!list.length) listBox.appendChild(h("p", { class: "muted small", text: n.kind === "local" ? "None set: agents use the folders indra was started with." : "None yet: add one below." }));
        list.forEach(function (f, i) {
          listBox.appendChild(h("div", { class: "row folder" }, h("span", { class: "cmd", text: f }),
            h("button", { type: "button", class: "ghost", "aria-label": "Remove " + f, onclick: function () { list.splice(i, 1); drawList(); } }, "Remove")));
        });
      }
      function add(f) { f = (f || "").trim(); if (f && list.indexOf(f) < 0) list.push(f); drawList(); }
      function browse(path) {
        browser.textContent = "";
        browser.appendChild(h("p", { class: "muted small", text: "Opening..." }));
        api("/api/nodes/" + encodeURIComponent(n.name) + "/dirs" + (path ? q({ path: path }) : "")).then(function (d) {
          browser.textContent = "";
          var head = h("div", { class: "row" }, h("b", { class: "cmd", text: d.path }),
            h("button", { type: "button", class: "primary", onclick: function () { add(d.path); browser.textContent = ""; } }, "Use this folder"));
          var nav = h("div", { class: "chips" },
            d.parent ? h("button", { type: "button", class: "ghost", onclick: function () { browse(d.parent); } }, "Up") : null,
            d.roots.map(function (r) { return h("button", { type: "button", class: "ghost", title: r.path, onclick: function () { browse(r.path); } }, r.label); }));
          var dirs = h("div", { class: "dirlist" }, d.dirs.length ? d.dirs.map(function (x) {
            return h("button", { type: "button", class: "ghost dir", onclick: function () { browse(d.path.replace(/[\\/]$/, "") + d.sep + x); } }, x);
          }) : h("p", { class: "muted small", text: "No folders inside." }));
          browser.appendChild(h("div", { class: "card browser" }, head, nav, dirs, d.more ? h("p", { class: "muted small", text: "Showing the first 500." }) : null));
        }).catch(function (e) { browser.textContent = ""; browser.appendChild(h("div", { class: "note bad", text: e.message })); });
      }
      box.textContent = "";
      drawList();
      box.appendChild(h("div", { class: "card form folders" },
        h("h3", { text: "Folders agents work in on " + n.name }),
        h("p", { class: "muted small", text: "Agents can only be started in these folders. They do not have to be where indra is installed." }),
        listBox,
        h("div", { class: "row" }, typed, h("button", { type: "button", onclick: function () { add(typed.value); typed.value = ""; } }, "Add"),
          n.info ? h("button", { type: "button", onclick: function () { browse(""); } }, "Browse...") : null),
        browser, err,
        h("div", { class: "row" }, h("button", { type: "button", class: "primary", onclick: function (e) {
          if (typed.value.trim()) { add(typed.value); typed.value = ""; }
          e.target.disabled = true; err.hidden = true;
          api("/api/nodes/" + encodeURIComponent(n.name) + "/folders", { method: "PUT", body: { folders: list } }).then(function (r) {
            toast("Saved: " + (r.folders.length ? r.folders.join(", ") : "default folders")); load(true);
          }).catch(function (x) { err.textContent = x.message; err.hidden = false; e.target.disabled = false; });
        } }, "Save"), h("button", { type: "button", onclick: function () { box.textContent = ""; } }, "Cancel"))));
      typed.focus();
    }
    function remove(n) {
      if (!confirm("Remove " + n.name + "? It disconnects now; to add it again, run a new join command on it.")) return;
      api("/api/nodes/" + encodeURIComponent(n.name), { method: "DELETE" }).then(function () { toast("Removed " + n.name); load(true); })
        .catch(function (e) { toast(e.message, true); });
    }
    function render(nodes) {
      grid.textContent = "";
      nodes.forEach(function (n) {
        var i = n.info, ok = !!i, edBox = h("div");
        var state = n.kind === "local" ? "this machine" : n.transport === "webrtc" ? "connected (direct)" : n.transport === "relay" ? "connected (relayed)" : n.transport === "none" ? "offline" : "over SSH";
        var card = h("div", { class: "card" },
          h("div", { class: "row" }, h("h4", { text: n.name }), h("span", { class: "pill" + (n.transport === "webrtc" ? " ok" : ""),
            title: { webrtc: "Dialled in; a direct, encrypted WebRTC data channel", relay: "Dialled in; traffic relayed over its link to this hub", ssh: "Reached over SSH", none: "Not connected" }[n.transport] || "",
            text: state }),
            n.transport === "none" ? null : h("span", { class: "pill " + (ok ? "ok" : "bad"), text: ok ? "online" : "unreachable" })),
          n.kind === "local" ? h("p", { class: "muted", text: n.description || "" }) : (n.ssh || (n.description && n.description !== "joined with a code")) ? h("p", { class: "muted", text: [n.ssh, n.description === "joined with a code" ? "" : n.description].filter(Boolean).join("  -  ") }) : null,
          ok ? h("div", { class: "stats" },
            [["computer", i.hostname], ["system", i.os + " " + i.arch], ["cpus", String(i.cpus)], ["indra", i.version]].map(function (x) {
              return h("div", { class: "stat" }, h("b", { class: "small", text: x[1] }), h("span", { text: x[0] }));
            })) : h("div", { class: "note bad" }, n.ssh ? (n.error || "not checked yet") : "Not connected. It reconnects by itself when it is back on. On that machine, `indra node status` says why, and `indra node connect` starts the link.",
              !n.ssh && canJoin() ? h("div", { class: "row" }, h("button", { type: "button", class: "ghost", title: "If it left or was reinstalled: a new command to join again", onclick: function () { addMachine(); } }, "Get a new join command")) : null),
          ok && n.kind !== "local" && i.version && olderThan(i.version, S.ov.version) ? h("div", { class: "note warn" },
            "This machine runs indra " + i.version + " and the hub " + S.ov.version + ", so some things here will not work. On it, run ", h("code", { text: "indra update" }), ", then ", h("code", { text: "indra node restart" }), ".") : null,
          meter(n.running, n.max_parallel),
          ok ? [h("h3", { text: "Agents installed" }), h("div", { class: "chips" }, i.runners.map(function (r) { return h("span", { class: "pill " + (r.available ? "ok" : ""), text: r.name + (r.available ? "" : " (not installed)") }); }))] : null,
          h("div", { class: "row" }, h("h3", { text: "Folders agents work in" }),
            canJoin() && (n.kind === "local" || ok || n.ssh) ? h("button", { type: "button", class: "ghost", onclick: function () { folderEditor(n, edBox); } }, "Change") : null),
          n.targets.length ? h("div", { class: "files" }, n.targets.map(function (t) { return h("div", { class: "cmd", text: t }); })) : h("p", { class: "muted", text: "none yet" }),
          edBox,
          h("div", { class: "row" }, h("a", { class: "btn", href: "#/launch", text: "Run an agent here" }),
            h("button", { type: "button", onclick: function () { showRuns(n); } }, "Results here"),
            n.kind !== "local" && canJoin() && (!n.ssh || canEdit()) ? h("button", { type: "button", class: "ghost", onclick: function () { remove(n); } }, "Remove") : null));
        grid.appendChild(card);
      });
      if (nodes.length === 1) grid.appendChild(h("div", { class: "card" }, h("h4", { text: "Add another machine" }),
        h("p", { class: "muted", text: "Run agents on a laptop, a build box or a server. Click Add a machine, copy one command, run it there: done." }),
        canJoin() ? h("button", { type: "button", class: "primary", onclick: function () { addMachine(); } }, "Add a machine")
          : h("p", { class: "muted small", text: "Ask an admin of this console to add it." })));
    }
    function load(force) { grid.textContent = ""; grid.appendChild(h("div", { class: "muted", text: "Checking machines..." })); loadNodes(force).then(render).catch(function (e) { grid.textContent = e.message; }); }
    refresh.addEventListener("click", function () { load(true); });
    load(false);
  }

  // ---- workflows ----------------------------------------------------------------
  var WF_STATE = { waiting: "", queued: "warn", running: "run live", succeeded: "ok", failed: "bad", skipped: "", cancelled: "warn" };
  function wfPill(s) { return h("span", { class: "pill " + (WF_STATE[s] || ""), text: s }); }

  function pageWorkflows(m) {
    var ov = S.ov, hs = harness();
    store("indra.seen.workflows", "1");
    var genBox = h("div");
    m.appendChild(header("Workflows", "Several steps that run in order: agents, skills and commands, on any of your machines. Build one in the editor or describe it and let Claude build it; run it here or with `indra run <name>`.",
      [canEdit() && !ov.read_only ? h("button", { type: "button", onclick: function () { showGen(); } }, "Create with Claude") : null,
       canEdit() ? h("a", { class: "btn", href: "#/wfedit/", text: "New workflow" }) : null]));
    m.appendChild(genBox);
    var waitingFor = null; // names before a generated workflow, to spot the new one
    function showGen() {
      genBox.textContent = "";
      var desc = h("textarea", { "aria-label": "Describe the workflow", placeholder: "e.g. Every night: pull, run the tests; if they fail, have an agent fix them and run them again (at most 3 times); then review the diff and post a summary." });
      var where = h("select", { "aria-label": "Save in" }, h("option", { value: "config", text: "My indra.toml (only on this machine)" }));
      var go = h("button", { class: "primary", type: "submit" }, "Create");
      var note = h("p", { class: "muted small", text: "Claude Code builds it with indra's MCP server: it reads the format and this machine's profiles, nodes and skills, validates the workflow until it is right, and saves it. You can then edit it in the graph editor." });
      genBox.appendChild(h("form", { class: "card form genbox", onsubmit: function (e) {
        e.preventDefault(); go.disabled = true;
        waitingFor = Array.prototype.map.call(defsBox.querySelectorAll("h4"), function (x) { return x.textContent; });
        api("/api/workflow-files/generate", { method: "POST", body: { description: desc.value, location: where.value } }).then(function (r) {
          genBox.textContent = "";
          genBox.appendChild(h("div", { class: "note" }, "Claude is writing the workflow (", h("a", { href: "#/proc/" + r.proc, text: "follow it, or answer its questions" }), "). It appears below once saved."));
        }).catch(function (err) { toast(err.message, true); go.disabled = false; });
      } }, h("h3", { text: "Create a workflow with Claude" }), h("label", null, "What should it do?", desc), h("label", null, "Save in", where), note,
        h("div", { class: "row" }, go, h("button", { type: "button", onclick: function () { genBox.textContent = ""; } }, "Cancel"))));
      api("/api/config").then(function (c) {
        ((c.repo && c.repo.repos) || []).forEach(function (r) { where.appendChild(h("option", { value: r.dir, text: r.dir + " (commit to share)" })); });
      }).catch(function () { /* config only */ });
      desc.focus();
    }
    var runsBox = h("div"), defsBox = h("div", { class: "grid" });
    var defsSig = null;
    m.appendChild(h("h2", { text: "Runs" })); m.appendChild(runsBox);
    var errBox = h("div");
    m.appendChild(h("h2", { text: "Workflows" })); m.appendChild(errBox); m.appendChild(defsBox);
    function loadRuns() {
      api("/api/workflows").then(function (w) {
        runsBox.textContent = "";
        if (!w.runs.length) runsBox.appendChild(empty("No workflow has run since the console started."));
        else runsBox.appendChild(h("div", { class: "tablewrap" }, h("table", null,
          h("thead", null, h("tr", null, ["#", "Workflow", "Status", "Tasks", "Started"].map(function (t) { return h("th", { text: t }); }))),
          h("tbody", null, w.runs.map(function (r) {
            var go = function () { location.hash = "#/workflow/" + r.id; };
            var done = r.tasks.filter(function (t) { return t.state === "succeeded"; }).length;
            return h("tr", { class: "click", tabindex: "0", onclick: go, onkeydown: function (e) { if (e.key === "Enter") go(); } },
              h("td", { text: String(r.id) }), h("td", null, h("b", { text: r.name })), h("td", null, wfPill(r.status)),
              h("td", null, h("div", { class: "segs" }, r.tasks.map(function (t) { return h("i", { class: t.state === "succeeded" ? "done" : t.state === "running" ? "running" : "", title: t.id + ": " + t.state }); })), h("span", { class: "muted", text: " " + done + "/" + r.tasks.length })),
              h("td", { class: "muted", text: ago(r.started) }));
          })))));
        // Redrawn only when they change, so an open Run form survives the refresh.
        var sig = JSON.stringify([w.defs, w.errors]);
        if (sig === defsSig) return;
        if (waitingFor && defsSig !== null) {
          var fresh = w.defs.filter(function (d) { return waitingFor.indexOf(d.name) < 0; });
          if (fresh.length) {
            waitingFor = null;
            genBox.textContent = "";
            genBox.appendChild(h("div", { class: "note" }, "Workflow ", h("b", { text: fresh[0].name }), " is ready. ",
              h("a", { href: "#/wfedit/" + encodeURIComponent(fresh[0].name), text: "Open it in the editor" }), " or run it below."));
          }
        }
        defsSig = sig;
        defsBox.textContent = ""; errBox.textContent = "";
        if ((w.errors || []).length) errBox.appendChild(h("div", { class: "note warn" }, h("b", { text: "Workflow files in repositories that are not used:" }),
          h("ul", null, w.errors.map(function (e) { return h("li", { class: "small", text: e }); })),
          h("span", { class: "small", text: "A workflow shared in a repository can name a profile or node only its author has: add one with that name (Settings), or edit the file." })));
        if (!w.defs.length) defsBox.appendChild(h("div", { class: "card" }, h("h4", { text: "No workflows configured" }),
          h("p", { class: "muted", text: "Chain agent tasks across machines, in dependency order. Or use a fan-out below." }),
          canEdit() ? h("a", { class: "btn", href: "#/wfedit/", text: "Create a workflow" }) : null));
        w.defs.forEach(function (d) {
          var formBox = h("div");
          defsBox.appendChild(h("div", { class: "card" },
            h("div", { class: "row" }, h("h4", { text: d.name }), h("span", { class: "pill", text: d.task.length + (d.task.length === 1 ? " task" : " tasks") }),
              d.builtin ? h("span", { class: "pill", text: "built in" }) : d.source ? h("span", { class: "pill ok", title: d.source, text: "in a repository" }) : h("span", { class: "pill", title: "In this machine's indra.toml", text: "local" }),
              d.schedule ? h("span", { class: "pill run", title: "Runs by itself while the console runs (" + d.schedule + ")", text: "\u23f0 " + (d.schedule_text || d.schedule) }) : null),
            d.description ? h("p", { class: "muted", text: d.description }) : null,
            d.source ? h("p", { class: "muted small clip1", title: d.source }, h("code", { text: d.source })) : null,
            h("ul", { class: "links" }, d.task.map(function (t) {
              var how = t["if"] != null ? "  If " + condText(t["if"]) + "?" : "  on " + (t.node || "local") + (t.branch ? " (on " + (t.branch === "yes" ? "Yes" : "No") + ")" : "") +
                (t.foreach != null ? ", for each item" : "") + (t.until != null ? ", repeated" : "");
              return h("li", null, h("b", { text: t.id }), h("span", { class: "muted", text: how + ((t.needs || []).length ? ", after " + t.needs.join(", ") : "") }));
            })),
            (d.input || []).length ? h("p", { class: "muted small", text: "Inputs: " + d.input.map(function (i) { return (i.label || i.name) + (i.required ? " *" : ""); }).join(", ") }) : null,
            h("div", { class: "row" }, h("button", { class: "primary", type: "button", disabled: ov.read_only, onclick: function (e) {
              // With inputs: ask for them; without: run it.
              if ((d.input || []).length) { formBox.textContent = ""; formBox.appendChild(workflowForm(d)); return; }
              e.target.disabled = true;
              api("/api/workflows", { method: "POST", body: { name: d.name } }).then(function (r) { location.hash = "#/workflow/" + r.id; })
                .catch(function (err) { toast(err.message, true); e.target.disabled = false; });
            } }, "Run"), canEdit() && !d.builtin ? h("a", { href: "#/wfedit/" + encodeURIComponent(d.name), text: "Edit graph" }) : null), formBox));
        });
      }).catch(function (e) { runsBox.textContent = e.message; });
    }
    loadRuns(); every(3000, loadRuns);

    if (ov.read_only || !hs) return;
    m.appendChild(h("h2", { id: "fanout", text: "Run on many folders at once" }));
    var fan = h("div", { class: "card" }, h("p", { class: "muted", text: "Loading nodes..." }));
    m.appendChild(fan);
    loadNodes(false).then(function (nodes) {
      fan.textContent = "";
      var prompt = h("textarea", { placeholder: "What should the agent do in each folder? {target} is the folder." });
      var names = {}; nodes.forEach(function (n) { runnersOn(n, ov).forEach(function (r) { names[r] = 1; }); });
      var runner = h("select", null, Object.keys(names).map(function (r) { return h("option", { value: r, text: r }); }));
      var fprofile = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: "None" }), (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); }));
      var boxes = [];
      var picks = h("div", { class: "picks" }, nodes.map(function (n) {
        return h("fieldset", null, h("legend", null, n.name, " ", h("span", { class: "muted", text: n.error ? "(offline)" : "(" + n.running + " of " + n.max_parallel + " running)" })),
          n.targets.length ? n.targets.map(function (t, i) {
            var cb = h("input", { type: "checkbox" }); boxes.push({ cb: cb, node: n.name, target: i });
            return h("label", { class: "pick" }, cb, h("span", { text: t }));
          }) : h("p", { class: "muted", text: "no folders yet" }));
      }));
      var go = h("button", { class: "primary", type: "submit" }, "Run in the chosen folders");
      fan.appendChild(h("form", { class: "form", onsubmit: function (e) {
        e.preventDefault();
        var targets = boxes.filter(function (b) { return b.cb.checked; }).map(function (b) { return { node: b.node, target: b.target }; });
        if (!targets.length) { toast("Choose at least one folder", true); return; }
        go.disabled = true;
        api("/api/workflows/fanout", { method: "POST", body: { h: hs.name, runner: runner.value, prompt: prompt.value, targets: targets, profile: fprofile.value || null } })
          .then(function (r) { location.hash = "#/workflow/" + r.id; }).catch(function (err) { toast(err.message, true); go.disabled = false; });
      } },
        h("p", { class: "muted small", text: "The same task in each folder you choose, side by side (as many at once as each machine allows)." }),
        h("label", null, "What should it do?", prompt),
        h("div", { class: "row" }, h("label", null, "Agent", runner), h("label", null, "Profile", fprofile)), h("div", null, h("b", { text: "Folders" }), picks), h("div", { class: "row" }, go)));
    }).catch(function (e) { fan.textContent = e.message; });
  }

  function pageWorkflowRun(m, id) {
    var top = h("div"), dag = h("div", { class: "dag" });
    m.appendChild(top); m.appendChild(dag);
    function depth(tasks) {
      var by = {}, memo = {};
      tasks.forEach(function (t) { by[t.id] = t; });
      function d(id) { if (memo[id] != null) return memo[id]; var t = by[id]; memo[id] = 0; memo[id] = t && t.needs.length ? 1 + Math.max.apply(null, t.needs.map(d)) : 0; return memo[id]; }
      tasks.forEach(function (t) { d(t.id); });
      return memo;
    }
    function load() {
      api("/api/workflows/" + id).then(function (r) {
        top.textContent = "";
        var cancel = h("button", { class: "danger", type: "button", disabled: r.status !== "running" || S.ov.read_only, onclick: function () {
          if (!window.confirm("Cancel this workflow? Its running steps are stopped.")) return;
          cancel.disabled = true; api("/api/workflows/" + id + "/cancel", { method: "POST" }).then(function () { toast("Cancelling"); }).catch(function (e) { toast(e.message, true); });
        } }, "Cancel workflow");
        top.appendChild(header("Workflow #" + r.id + " - " + r.name, "Started " + ago(r.started) + (r.ended ? ", finished " + ago(r.ended) : "") + (r.stopped_by ? ", ended early by step " + r.stopped_by : ""), [wfPill(r.status), r.status === "running" ? cancel : (S.ov.read_only ? null : h("button", { type: "button", class: "primary", title: "Run this workflow again with the same inputs", onclick: function (e) {
          e.target.disabled = true;
          api("/api/workflows", { method: "POST", body: { name: r.name, inputs: r.inputs || {} } }).then(function (x) { location.hash = "#/workflow/" + x.id; })
            .catch(function (err) { toast(err.message, true); e.target.disabled = false; });
        } }, "Run again"))]));
        var lv = depth(r.tasks), cols = [];
        r.tasks.forEach(function (t) { (cols[lv[t.id]] = cols[lv[t.id]] || []).push(t); });
        dag.textContent = "";
        cols.forEach(function (col, ci) {
          dag.appendChild(h("div", { class: "dag-col" }, h("h3", { text: ci === 0 ? "Start" : "Step " + (ci + 1) }), col.map(function (t) {
            return h("div", { class: "card task " + t.state },
              h("div", { class: "row" }, h("b", { text: t.id }), wfPill(t.state), t.allow_failure ? h("span", { class: "pill", title: "Its failure does not fail the workflow", text: "may fail" }) : null),
              t["if"] != null ? h("div", null, h("span", { class: "muted", text: "If " + condText(t["if"]) + "? " }), t.output ? h("span", { class: "pill " + (t.output === "yes" ? "ok" : "bad"), text: t.output === "yes" ? "Yes" : "No" }) : null)
                : h("div", { class: "muted", text: t.node + "  -  " + (t.run ? "command" : t.runner) + (t.branch ? "  (on " + (t.branch === "yes" ? "Yes" : "No") + ")" : "") }),
              t.run ? h("div", { class: "cmd small", text: "$ " + t.run }) : null,
              h("div", { class: "cmd small", text: t.target }),
              t.item ? h("div", { class: "muted", text: "item: " + t.item }) : null,
              t.needs.length ? h("div", { class: "muted", text: (t.on && t.on !== "success" ? "on " + t.on + " of " : "after ") + t.needs.join(", ") }) : null,
              t.when ? h("div", { class: "muted small", text: "when " + t.when }) : null,
              t.until ? h("div", { class: "muted small", text: "until " + t.until + " (run " + t.runs + " of at most " + t.max_runs + ")" }) : null,
              t.attempts > 1 || t.retries ? h("div", { class: "muted", text: "attempt " + t.attempts + " of " + (t.retries + 1) }) : null,
              t.exit_code != null && t.state !== "running" ? h("div", { class: "muted small", text: "exit code " + t.exit_code }) : null,
              t.error ? h("div", { class: "note " + (t.state === "failed" ? "bad" : "warn"), text: t.error }) : null,
              t.output && t.state !== "running" ? h("details", { keep: "wf-out:" + r.id + ":" + t.id }, h("summary", { text: "Output" }), h("pre", { class: "view small", text: t.output.slice(-4000) })) : null,
              t.proc_id ? h("a", { href: "#/proc/" + t.proc_id, text: (t.run ? "Open command process #" : "Open agent process #") + t.proc_id }) : null);
          })));
        });
        if (r.status !== "running") stop();
      }).catch(function (e) { top.textContent = ""; top.appendChild(empty(e.message)); stop(); });
    }
    var t = setInterval(load, 1500);
    function stop() { clearInterval(t); }
    S.cleanup.push(stop);
    load();
  }

  function statusPill(s) {
    var cls = { running: "run live", exited: "ok", failed: "bad", stopped: "warn" }[s] || "";
    return h("span", { class: "pill " + cls, text: s });
  }
  function procTable(list) {
    return h("div", { class: "tablewrap" }, h("table", null,
      h("thead", null, h("tr", null, ["#", "Status", "Machine", "Agent", "Task", "Subagents", "Cost", "Started"].map(function (t) { return h("th", { text: t }); }))),
      h("tbody", null, list.map(function (p) {
        var go = function () { location.hash = "#/proc/" + p.id; };
        return h("tr", { class: "click", tabindex: "0", onclick: go, onkeydown: function (e) { if (e.key === "Enter") go(); } },
          h("td", { text: String(p.id) }), h("td", null, statusPill(p.status), p.pending && p.pending.length ? h("span", { class: "pill warn", text: "needs you (" + p.pending.length + ")" }) : null),
          h("td", { text: p.node + (p.host && p.node !== "local" ? " (" + p.host + ")" : "") }), h("td", { text: p.runner + (p.model ? " - " + p.model : "") }),
          h("td", { class: "desc" }, h("div", { text: p.prompt.length > 90 ? p.prompt.slice(0, 88) + "..." : p.prompt }), p.last ? h("div", { class: "muted", text: p.last }) : null),
          h("td", { text: String(p.agents.length) }),
          h("td", { text: p.cost_usd != null ? "$" + p.cost_usd.toFixed(4) : "-" }),
          h("td", { class: "muted", text: ago(p.started) }));
      }))));
  }

  // ---- marketplace -------------------------------------------------------------------
  // Plugins and skills from Claude Code plugin marketplaces (or skill repos),
  // installed where Claude Code, Codex or a harness will find them.
  function pageMarket(m, arg) {
    var parts = String(arg || "").split("/"), want = parts[0] || store("indra.market") || "", tab = parts[1] === "installed" ? "installed" : "browse";
    var ro = S.ov.read_only || !S.ov.is_admin, hs = harness();
    m.appendChild(header("Marketplace", "Plugins, skills and agents from Claude Code plugin marketplaces and skill repositories, installed for Claude Code, Codex or a harness. Fetching happens only when you press Refresh."));
    var top = h("div"), tabs = h("div"), body = h("div", { class: "stack" });
    m.appendChild(top); m.appendChild(tabs); m.appendChild(body);
    api("/api/market").then(function (r) {
      var list = r.marketplaces, cur = list.find(function (x) { return x.name === want; }) || list.find(function (x) { return x.plugins; }) || list[0];
      if (cur) store("indra.market", cur.name);
      top.appendChild(h("div", { class: "vaults" }, list.map(function (x) {
        var pick = function () { location.hash = "#/market/" + encodeURIComponent(x.name); };
        var st = x.state || {};
        var refresh = h("button", { type: "button", disabled: ro, onclick: function (e) {
          e.stopPropagation(); refresh.disabled = true; refresh.textContent = "Fetching...";
          api("/api/market/refresh", { method: "POST", body: { m: x.name } }).then(function () { toast("Fetched " + x.name); route(); })
            .catch(function (err) { toast(err.message, true); refresh.disabled = false; refresh.textContent = "Refresh"; });
        } }, x.plugins == null ? "Fetch" : "Refresh");
        return h("div", { class: "vault" + (cur && x.name === cur.name ? " sel" : ""), tabindex: "0", title: x.description || x.source, onclick: pick, onkeydown: function (e) { if (e.key === "Enter") pick(); } },
          h("b", { text: x.name }), x.builtin ? h("span", { class: "pill", text: "built in" }) : null,
          h("code", { class: "vpath", text: x.source + (x.ref ? "@" + x.ref : "") }),
          h("span", { class: "vstats" }, x.plugins == null ? h("span", { class: "muted", text: "not fetched" }) : h("span", null, h("b", { text: String(x.plugins) }), " plugins"),
            st.fetched ? h("span", { class: "muted", text: ago(st.fetched) }) : null, st.error ? h("span", { class: "warn", title: st.error, text: "fetch failed" }) : null),
          refresh);
      })));
      if (canEdit()) top.appendChild(h("details", { class: "addmk" }, h("summary", { text: "Add a marketplace" }), (function () {
        var name = h("input", { type: "text", placeholder: "my-team", required: true }), src = h("input", { type: "text", placeholder: "owner/repo, https://.../repo.git, or a folder", required: true }), ref = h("input", { type: "text", placeholder: "optional branch, tag or commit" });
        return h("form", { class: "form", onsubmit: function (e) {
          e.preventDefault();
          var v = { name: name.value.trim(), source: src.value.trim() }; if (ref.value.trim()) v.ref = ref.value.trim();
          addToSection("marketplace", v).then(function () { toast("Added " + v.name + "; press Fetch"); location.hash = "#/market/" + encodeURIComponent(v.name); route(); }).catch(function (err) { toast(err.message, true); });
        } }, h("div", { class: "fields" }, h("label", null, "Name", name), h("label", { class: "wide" }, "Source", src), h("label", null, "Ref", ref)),
          h("p", { class: "muted small", text: "Any Claude Code plugin marketplace (a repository with .claude-plugin/marketplace.json), or a repository of skills (folders with SKILL.md)." }),
          h("div", null, h("button", { type: "submit", class: "primary" }, "Add")));
      })()));
      tabs.appendChild(h("nav", { class: "tabs" },
        h("a", { href: "#/market/" + encodeURIComponent(cur ? cur.name : ""), class: tab === "browse" ? "on" : "", text: "Browse" }),
        h("a", { href: "#/market/" + encodeURIComponent(cur ? cur.name : "") + "/installed", class: tab === "installed" ? "on" : "" }, "Installed", r.installed.length ? h("b", { class: "badge", text: String(r.installed.length) }) : null)));
      if (tab === "installed") return installed(r.installed);
      if (!cur) { body.appendChild(empty("No marketplaces.")); return; }
      if (cur.plugins == null) { body.appendChild(empty(cur.state && cur.state.error ? "Fetching failed: " + cur.state.error : "Press Fetch to download " + cur.name + " (" + cur.source + ").")); return; }
      browse(cur);
    }).catch(function (e) { body.appendChild(empty(e.message)); });

    function browse(mk) {
      var search = h("input", { type: "search", placeholder: "Search plugins and skills", "aria-label": "Search" });
      var cat = h("select", { "aria-label": "Category" }, h("option", { value: "", text: "All categories" }));
      var list = h("div", { class: "klist" }), detail = h("div");
      body.appendChild(h("div", { class: "row" }, search, cat));
      body.appendChild(h("div", { class: "kgrid" }, list, detail));
      detail.appendChild(empty("Pick a plugin to see what is inside and install it."));
      api("/api/market/plugins" + q({ m: mk.name })).then(function (c) {
        var plugins = c.plugins;
        var cats = {}; plugins.forEach(function (p) { if (p.category) cats[p.category] = 1; });
        Object.keys(cats).sort().forEach(function (k) { cat.appendChild(h("option", { value: k, text: k })); });
        function draw() {
          var f = search.value.trim().toLowerCase();
          var rows = plugins.filter(function (p) { return (!cat.value || p.category === cat.value) && (!f || (p.name + " " + (p.description || "") + " " + (p.category || "")).toLowerCase().indexOf(f) >= 0); });
          list.textContent = "";
          if (!rows.length) { list.appendChild(empty("Nothing matches.")); return; }
          rows.slice(0, 400).forEach(function (p) {
            list.appendChild(h("a", { class: "kitem", href: "#", onclick: function (e) { e.preventDefault(); Array.prototype.forEach.call(list.children, function (x) { x.classList.remove("sel"); }); e.currentTarget.classList.add("sel"); show(mk, p); } },
              h("div", { class: "row" }, h("b", { text: p.name }), p.category ? h("span", { class: "pill", text: p.category }) : null,
                p.installed && p.installed.length ? h("span", { class: "pill ok", text: "installed" }) : null, !p.installable ? h("span", { class: "pill warn", text: "Claude Code only" }) : null),
              p.description ? h("div", { class: "muted small clip2", text: p.description }) : null));
          });
        }
        search.addEventListener("input", draw); cat.addEventListener("change", draw); draw();
      }).catch(function (e) { list.appendChild(empty(e.message)); });

      function show(mk, p) {
        detail.textContent = "";
        var inside = p.source_kind !== "here" ? h("div", { class: "muted small", text: "This plugin lives in another repository (" + p.source_kind + "); Look inside fetches it." }) : null;
        var card = h("div", { class: "card form" },
          h("div", { class: "row" }, h("h2", { text: p.name }), p.version ? h("span", { class: "pill", text: "v" + p.version }) : null, p.category ? h("span", { class: "pill", text: p.category }) : null),
          p.description ? h("p", { text: p.description }) : null,
          h("p", { class: "muted small" }, p.author ? "By " + p.author + ". " : "", safeUrl(p.homepage) ? h("a", { href: safeUrl(p.homepage), target: "_blank", rel: "noopener noreferrer", text: "Homepage" }) : null),
          inside);
        detail.appendChild(card);
        if (ro) return;
        if (!p.installable) { card.appendChild(nativeForm(mk, p, true)); return; }
        var look = h("button", { type: "button", class: "primary", onclick: function () {
          look.disabled = true; look.textContent = "Looking...";
          api("/api/market/inspect", { method: "POST", body: { m: mk.name, plugin: p.name } }).then(function (r) { look.remove(); installForm(card, mk, p, r); })
            .catch(function (err) { toast(err.message, true); look.disabled = false; look.textContent = "Look inside"; });
        } }, "Look inside");
        card.appendChild(h("div", { class: "row" }, look));
        card.appendChild(nativeForm(mk, p, false));
      }
    }

    function targetsSelect() {
      return h("select", { "aria-label": "Project" }, (hs ? hs.targets : []).map(function (t, i) { return h("option", { value: String(i), text: t }); }));
    }

    function installForm(card, mk, p, r) {
      if (!r.items.length) { card.appendChild(h("p", { class: "muted", text: "No skills, agents or commands to copy." + (r.native.length ? " It has " + r.native.join(", ") + ": install it with Claude Code below." : "") })); return; }
      var boxes = r.items.map(function (it) { var cb = h("input", { type: "checkbox", checked: true }); cb.dataset.key = it.kind + " " + it.name; return { it: it, cb: cb }; });
      var native = card.querySelector("details.native");
      var add = function (el) { card.insertBefore(el, native); };
      add(h("h3", { text: "Inside (" + r.items.length + ")" }));
      add(h("div", { class: "rowlist" }, boxes.map(function (b) {
        return h("label", { class: "check rowitem" }, b.cb, h("span", { class: "pill", text: b.it.kind }), h("span", { class: "grow" }, h("b", { text: b.it.name }), b.it.description ? h("span", { class: "muted small", text: "  " + b.it.description.slice(0, 160) }) : null));
      })));
      if (r.native.length) add(h("div", { class: "note warn", text: "It also has " + r.native.join(", ") + ", which only Claude Code's own plugin install sets up (below). Copying brings the skills, agents and commands." }));
      var dest = h("select", { "aria-label": "Install for" },
        [["claude-user", "Claude Code, for me (~/.claude)"], ["claude-project", "Claude Code, in a project (.claude)"], ["codex-user", "Codex, for me (~/.agents/skills, skills only)"],
          ["codex-project", "Codex, in a project (.agents/skills, skills only)"], ["harness", "The " + (hs ? hs.name : "") + " harness (its own folders)"], ["new-harness", "A new harness, read from this plugin"]]
          .map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
      var proj = targetsSelect(), projLabel = h("label", { hidden: true }, "Project", proj);
      dest.addEventListener("change", function () { projLabel.hidden = dest.value.indexOf("-project") < 0; });
      var overwrite = h("input", { type: "checkbox" });
      var go = h("button", { type: "button", class: "primary", onclick: function () {
        if (dest.value === "new-harness") {
          addToSection("harness", { name: p.name, root: r.root, agents: ["agents/*.md"], skills: ["skills/*/SKILL.md"], prompts: ["commands/*.md"] })
            .then(function () { toast("Added harness " + p.name); }).catch(function (err) { toast(err.message, true); });
          return;
        }
        go.disabled = true;
        api("/api/market/install", { method: "POST", body: { m: mk.name, plugin: p.name, dest: dest.value, h: S.h, target: Number(proj.value || 0),
          items: boxes.filter(function (b) { return b.cb.checked; }).map(function (b) { return b.cb.dataset.key; }), overwrite: overwrite.checked } })
          .then(function (x) { toast("Installed " + x.items.length + " item(s) under " + x.base); go.disabled = false; })
          .catch(function (err) { toast(err.message, true); go.disabled = false; });
      } }, "Install");
      add(h("h3", { text: "Install" }));
      add(h("div", { class: "fields" }, h("label", { class: "wide" }, "For", dest), projLabel));
      add(h("label", { class: "check" }, overwrite, h("span", { text: "Replace files that exist already" })));
      add(h("div", { class: "note warn", text: "Install only from sources you trust: skills and agents tell agents what to do, and a skill's scripts run with your permissions when an agent uses them." }));
      add(h("div", { class: "row" }, go, r.commit ? h("span", { class: "muted small", text: "commit " + r.commit.slice(0, 10) }) : null));
    }

    function nativeForm(mk, p, only) {
      var scope = h("select", { "aria-label": "Scope" }, h("option", { value: "user", text: "for me (user)" }), h("option", { value: "project", text: "in a project" }));
      var proj = targetsSelect(), projLabel = h("label", { hidden: true }, "Project", proj);
      scope.addEventListener("change", function () { projLabel.hidden = scope.value !== "project"; });
      var out = h("pre", { class: "view small", hidden: true });
      var go = h("button", { type: "button", onclick: function () {
        go.disabled = true; go.textContent = "Installing...";
        api("/api/market/native", { method: "POST", body: { m: mk.name, plugin: p.name, scope: scope.value, h: S.h, target: Number(proj.value || 0) } })
          .then(function (x) { out.hidden = false; out.textContent = x.output || "done"; toast("Installed with Claude Code"); })
          .catch(function (err) { out.hidden = false; out.textContent = err.message; })
          .then(function () { go.disabled = false; go.textContent = "Install with Claude Code"; });
      } }, "Install with Claude Code");
      return h("details", { class: "native", open: only || null }, h("summary", { text: "Or install it as a Claude Code plugin (with its hooks and MCP servers)" }),
        h("p", { class: "muted small", text: "Runs claude plugin marketplace add, then claude plugin install " + p.name + "@<marketplace> on this machine. Claude Code then manages it (/plugin to see it)." }),
        h("div", { class: "fields" }, h("label", null, "Scope", scope), projLabel), h("div", { class: "row" }, go), out);
    }

    function installed(list) {
      if (!list.length) { body.appendChild(empty("Nothing installed from a marketplace yet.")); return; }
      body.appendChild(h("div", { class: "tablewrap" }, h("table", null,
        h("thead", null, h("tr", null, ["Plugin", "Where", "What", "Version", "When", ""].map(function (t) { return h("th", { text: t }); }))),
        h("tbody", null, list.slice().reverse().map(function (i) {
          return h("tr", null, h("td", null, h("b", { text: i.plugin }), h("div", { class: "muted small", text: i.marketplace })),
            h("td", null, h("div", { text: i.dest }), h("code", { class: "small", text: i.base })),
            h("td", { class: "desc", text: i.items.join(", ") }),
            h("td", { class: "muted", text: (i.version ? "v" + i.version + " " : "") + (i.commit ? i.commit.slice(0, 8) : "") }),
            h("td", { class: "muted", text: ago(i.installed) }),
            h("td", null, ro ? null : h("div", { class: "row" },
              h("button", { type: "button", onclick: function (e) {
                e.target.disabled = true;
                api("/api/market/installed/" + i.id + "/update", { method: "POST" }).then(function () { toast("Updated " + i.plugin); route(); }).catch(function (err) { toast(err.message, true); e.target.disabled = false; });
              } }, "Update"),
              h("button", { type: "button", class: "danger", onclick: function () {
                if (!window.confirm("Remove what was installed from " + i.plugin + " (" + i.items.length + " item(s) under " + i.base + ")?")) return;
                api("/api/market/installed/" + i.id, { method: "DELETE" }).then(function () { toast("Removed"); route(); }).catch(function (err) { toast(err.message, true); });
              } }, "Remove"))));
        })))));
    }
  }

  // ---- plan and delegate ------------------------------------------------------------
  // A strong model plans; cheaper ones (Haiku, a local model) do the steps.
  function launchTabs(on) {
    return h("nav", { class: "tabs", "aria-label": "Launch modes" },
      h("a", { href: "#/launch", class: on === "one" ? "on" : "", text: "One agent" }),
      h("a", { href: "#/delegate", class: on === "plan" ? "on" : "", text: "Plan and delegate" }));
  }
  function profileLabel(name) {
    var p = (S.ov.profiles || []).find(function (x) { return x.name === name; });
    if (!p) return name;
    return name + " (" + [p.runner || "claude", p.model || "default model"].join(", ") + ")";
  }
  function pageDelegate(m) {
    var ov = S.ov, hs = harness();
    m.appendChild(header("Plan and delegate", "A strong model reads the code and writes a plan; cheaper models carry out each step; an optional reviewer checks the result. Each role is a profile, so any runner works: Claude Code, Codex, Gemini CLI, or a local model through Ollama."));
    m.appendChild(launchTabs("plan"));
    if (ov.read_only) { m.appendChild(h("div", { class: "note warn", text: "indra is running read-only, so launching is disabled." })); return; }
    if (!hs) { m.appendChild(empty("No project yet: start indra in your project's folder (cd my-project, then indra serve), or pass --target <folder>.")); return; }
    var profiles = (ov.profiles || []).map(function (p) { return p.name; }), teams = ov.teams || [];
    if (profiles.length < 1) {
      m.appendChild(h("div", { class: "note warn" }, "Plan and delegate needs profiles for its roles. ", canEdit() ? h("a", { href: "#/settings/teams", text: "Set up a team in Settings > Teams" }) : "Add [[profile]] and [[team]] sections to indra.toml.", " (templates included)."));
      return;
    }
    var holder = h("div", { class: "muted", text: "Checking machines..." }); m.appendChild(holder);
    loadNodes(false).then(function (nodes) {
      holder.remove();
      var sel = function (label, list, blank) { return h("select", { "aria-label": label }, blank ? h("option", { value: "", text: blank }) : null, list.map(function (x) { return h("option", { value: x, text: profileLabel(x) }); })); };
      var team = h("select", { "aria-label": "Team" }, h("option", { value: "", text: "Pick the roles below" }), teams.map(function (t) { return h("option", { value: t.name, text: t.name + (t.description ? "  -  " + t.description : "") }); }));
      var planner = sel("Planner", profiles), worker = sel("Worker", profiles), light = sel("Light steps", profiles, "the worker"), reviewer = sel("Reviewer", profiles, "no review step");
      var review = h("input", { type: "checkbox", checked: true }), parallel = h("input", { type: "checkbox" });
      var maxSteps = h("input", { type: "number", min: "1", max: "30", value: "8" });
      var guess = function (re) { return profiles.find(function (p) { return re.test(p); }); };
      planner.value = guess(/plan/i) || profiles[0]; worker.value = guess(/work|build|haiku|local/i) || profiles[0];
      var roleBox = h("div", { class: "fields" }, h("label", null, "Planner", h("small", { text: "Reads and plans; changes nothing" }), planner),
        h("label", null, "Worker", h("small", { text: "Does each step" }), worker),
        h("label", null, "Light steps", h("small", { text: "Mechanical edits, cheapest model" }), light),
        h("label", null, "Reviewer", h("small", { text: "Checks the result at the end" }), reviewer));
      var opts2 = h("div", { class: "fields" }, h("label", null, "Most steps", maxSteps));
      function fillTeam() {
        var t = teams.find(function (x) { return x.name === team.value; });
        roleBox.hidden = opts2.hidden = !!t;
        if (t) { review.checked = !t.auto_run; parallel.checked = !!t.parallel; teamNote.textContent = "Planner " + profileLabel(t.planner) + "; worker " + profileLabel(t.worker) + (t.light_worker ? "; light steps " + profileLabel(t.light_worker) : "") + (t.reviewer ? "; reviewer " + profileLabel(t.reviewer) : "; no review step") + "."; }
        else teamNote.textContent = "";
      }
      var teamNote = h("p", { class: "muted small" });
      team.addEventListener("change", fillTeam);
      var node = h("select", null, nodes.map(function (n) { return h("option", { value: n.name, text: n.name + (n.kind === "local" ? "  (this machine)" : "") }); }));
      var target = h("select");
      function fillNode() { var n = nodes.find(function (x) { return x.name === node.value; }) || nodes[0]; target.textContent = ""; n.targets.forEach(function (t, i) { target.appendChild(h("option", { value: String(i), text: t })); }); }
      node.addEventListener("change", fillNode);
      var prompt = h("textarea", { placeholder: "What should be built or changed? The planner turns it into steps.", "aria-label": "Task" });
      var go = h("button", { class: "primary", type: "submit" }, "Plan it");
      m.appendChild(h("form", { class: "form", onsubmit: function (e) {
        e.preventDefault(); if (!prompt.value.trim()) { toast("Describe the task first", true); return; }
        go.disabled = true;
        var body = { h: hs.name, node: node.value, target: Number(target.value), prompt: prompt.value, auto_run: !review.checked };
        if (team.value) body.team = team.value;
        else body.roles = { planner: planner.value, worker: worker.value, light_worker: light.value || null, reviewer: reviewer.value || null, parallel: parallel.checked, auto_run: !review.checked, max_steps: Number(maxSteps.value) || 8 };
        api("/api/delegations", { method: "POST", body: body }).then(function (r) { location.hash = "#/plan/" + r.id; })
          .catch(function (err) { toast(err.message, true); go.disabled = false; });
      } },
        h("label", null, "Task", prompt),
        h("div", { class: "fields" }, h("label", null, "Machine", node), h("label", { class: "wide" }, "Folder", target)),
        h("label", null, "Team", h("small", { text: "A saved set of roles (Settings > Teams), or pick them here." }), team), teamNote,
        roleBox, opts2,
        h("label", { class: "check" }, review, h("span", null, h("b", { text: "Let me review the plan before it runs" }), h("small", { text: " - edit, drop or add steps first" }))),
        h("label", { class: "check" }, parallel, h("span", null, h("b", { text: "Independent steps may run at the same time" }), h("small", { text: " - they share the directory, so only when the plan keeps them apart" }))),
        h("div", { class: "note", text: "Tokens: the planner reads and plans once; every step then runs on the worker's model with a short, self-contained brief, so the expensive model never does the typing. Costs per role show on the plan's page." }),
        h("div", { class: "row" }, go, canEdit() ? h("a", { href: "#/settings/teams", text: "Save roles as a team" }) : null)));
      fillNode(); fillTeam();
    }).catch(function (e) { holder.textContent = e.message; });
  }

  var PLAN_STATE = { planning: "run live", review: "warn", running: "run live", done: "ok", failed: "bad", cancelled: "" };
  function pagePlan(m, id) {
    var top = h("div", { class: "stack" }), body = h("div", { class: "stack" });
    m.appendChild(h("div", { class: "stack" }, top, body));
    var editing = false;
    function load() {
      if (editing) return;
      api("/api/delegations/" + id).then(function (r) { render(r); }).catch(function (e) { top.textContent = ""; top.appendChild(empty(e.message)); });
    }
    function render(r) {
      var d = r.delegation, ro = S.ov.read_only;
      top.textContent = ""; body.textContent = "";
      var cancel = ["planning", "review", "running"].indexOf(d.status) >= 0 && !ro ? h("button", { type: "button", class: "danger", onclick: function () {
        if (!window.confirm("Cancel this plan?")) return;
        api("/api/delegations/" + id + "/cancel", { method: "POST" }).then(load).catch(function (e) { toast(e.message, true); });
      } }, "Cancel") : null;
      top.appendChild(header("Plan #" + d.id + (d.team ? " - team " + d.team : ""), (d.node !== "local" ? d.node + ":" : "") + d.target,
        [h("span", { class: "pill " + (PLAN_STATE[d.status] || ""), text: d.status }), r.total_cost_usd ? h("span", { class: "pill", text: "$" + r.total_cost_usd.toFixed(4) }) : null, cancel]));
      top.appendChild(h("div", { class: "cmd lines", text: d.task }));
      var roles = d.roles;
      body.appendChild(h("div", { class: "card" }, h("h3", { text: "Roles" }), h("div", { class: "roles" },
        [["planner", roles.planner], ["worker", roles.worker], ["light steps", roles.light_worker || roles.worker], ["reviewer", roles.reviewer || "none"]].map(function (x) {
          var c = r.cost[x[0] === "light steps" ? "light_worker" : x[0]];
          return h("div", { class: "stat" }, h("span", { text: x[0] }), h("b", { class: "small", text: x[1] === "none" ? "none" : profileLabel(x[1]) }), c ? h("span", { text: "$" + c.toFixed(4) }) : null);
        }))));
      if (d.error) body.appendChild(h("div", { class: "note bad", text: d.error }));
      if (d.status === "planning") {
        body.appendChild(h("div", { class: "card" }, h("p", null, "The planner is reading the code and writing the plan. "),
          d.planner_proc ? h("a", { href: "#/proc/" + d.planner_proc, text: "Watch it (process #" + d.planner_proc + ")" }) : null));
        return;
      }
      if (d.planner_proc) body.appendChild(h("p", { class: "muted small" }, "Planned by ", h("a", { href: "#/proc/" + d.planner_proc, text: "process #" + d.planner_proc }), r.planner && r.planner.cost_usd != null ? " ($" + r.planner.cost_usd.toFixed(4) + ")" : ""));
      if (!d.plan) return;
      if (d.status === "review" && !ro) return reviewForm(d);
      if (d.plan.summary) body.appendChild(h("div", { class: "card" }, h("h3", { text: "Approach" }), h("p", { text: d.plan.summary })));
      var byId = {}; r.steps.forEach(function (s) { byId[s.id] = s; });
      var list = d.plan.steps.map(function (s, i) { return { n: i + 1, s: s, run: byId[s.id] }; });
      if (byId.review) list.push({ n: "", s: { id: "review", title: "Review the result", size: "review", prompt: "" }, run: byId.review });
      body.appendChild(h("div", { class: "card" }, h("h3", { text: "Steps" }), h("div", { class: "rowlist" }, list.map(function (x) {
        var st = x.run ? x.run.state : "waiting";
        return h("div", { class: "rowitem" }, h("b", { text: x.n ? x.n + "." : "" }), h("span", { class: "pill " + ({ succeeded: "ok", failed: "bad", running: "run live", cancelled: "", skipped: "warn" }[st] || ""), text: st }),
          h("span", { class: "grow" }, h("b", { text: x.s.title }), h("span", { class: "muted small", text: "  " + (x.run ? profileLabel(x.run.profile || "") : x.s.size) })),
          x.run && x.run.cost_usd ? h("span", { class: "muted small", text: "$" + x.run.cost_usd.toFixed(4) }) : null,
          x.run && x.run.proc ? h("a", { href: "#/proc/" + x.run.proc, text: "process #" + x.run.proc }) : null);
      })), d.workflow ? h("p", { class: "muted small" }, h("a", { href: "#/workflow/" + d.workflow, text: "Open it as workflow run #" + d.workflow })) : null));
    }
    function reviewForm(d) {
      editing = true;
      var plan = JSON.parse(JSON.stringify(d.plan));
      var summary = h("textarea", { rows: "3", "aria-label": "Approach" }); summary.value = plan.summary || "";
      var stepsBox = h("div", { class: "stack" });
      function draw() {
        stepsBox.textContent = "";
        plan.steps.forEach(function (s, i) {
          var title = h("input", { type: "text", value: s.title, "aria-label": "Title" });
          var p = h("textarea", { rows: "5", "aria-label": "Instructions" }); p.value = s.prompt;
          var size = h("select", { "aria-label": "Who does it" }, h("option", { value: "standard", text: "worker: " + profileLabel(d.roles.worker) }),
            h("option", { value: "light", text: "light steps: " + profileLabel(d.roles.light_worker || d.roles.worker) })); size.value = s.size;
          var needs = h("input", { type: "text", value: (s.needs || []).join(", "), placeholder: "ids of steps that come first", "aria-label": "After" });
          [title, p, size, needs].forEach(function (el) { el.addEventListener("input", sync); el.addEventListener("change", sync); });
          function sync() { s.title = title.value; s.prompt = p.value; s.size = size.value; s.needs = csv(needs.value); }
          stepsBox.appendChild(h("div", { class: "card form step" },
            h("div", { class: "row" }, h("b", { text: (i + 1) + "." }), h("code", { text: s.id }), h("span", { class: "grow" }),
              h("button", { type: "button", class: "ghost", disabled: i === 0, onclick: function () { plan.steps.splice(i - 1, 0, plan.steps.splice(i, 1)[0]); draw(); } }, "Up"),
              h("button", { type: "button", class: "ghost", disabled: i === plan.steps.length - 1, onclick: function () { plan.steps.splice(i + 1, 0, plan.steps.splice(i, 1)[0]); draw(); } }, "Down"),
              h("button", { type: "button", class: "ghost danger", onclick: function () { plan.steps.splice(i, 1); draw(); } }, "Remove")),
            h("label", null, "Step", title), h("label", null, "Instructions for the worker", p),
            h("div", { class: "fields" }, h("label", null, "Who does it", size), h("label", null, "After", needs))));
        });
      }
      draw();
      body.appendChild(h("div", { class: "card form" }, h("h3", { text: "The planner's approach" }), summary));
      body.appendChild(h("h2", { text: "Steps (" + plan.steps.length + ")" }));
      body.appendChild(stepsBox);
      body.appendChild(h("div", { class: "row" },
        h("button", { type: "button", onclick: function () { plan.steps.push({ id: "step-" + (plan.steps.length + 1), title: "", prompt: "", needs: [], size: "standard" }); draw(); } }, "Add a step"),
        h("button", { type: "button", class: "primary", onclick: function (e) {
          plan.summary = summary.value; e.target.disabled = true;
          api("/api/delegations/" + d.id + "/approve", { method: "POST", body: { plan: plan } })
            .then(function () { editing = false; toast("Running the plan"); load(); })
            .catch(function (err) { toast(err.message, true); e.target.disabled = false; });
        } }, "Run the plan")));
    }
    load(); every(2500, load);
  }

  // ---- sessions: a conversation view of agents, like Claude Code online ----------
  // A session is an agent and the agents continued from it (a chain of
  // processes linked by `parent`). The page lists sessions on the left, shows
  // the selected one as a conversation in the middle, and what it works on
  // (machine, branch, context, tasks, changed files, links) on the right.
  function sessionChains(procs) {
    var by = {}; procs.forEach(function (p) { by[p.id] = p; });
    function root(p) { var seen = {}; while (p.parent && by[p.parent] && !seen[p.id]) { seen[p.id] = 1; p = by[p.parent]; } return p; }
    var chains = {};
    procs.forEach(function (p) { var r = root(p); (chains[r.id] = chains[r.id] || []).push(p); });
    return Object.keys(chains).map(function (k) {
      var list = chains[k].sort(function (a, b) { return a.started - b.started || a.id - b.id; });
      return { root: list[0], last: list[list.length - 1], procs: list };
    }).sort(function (a, b) { return b.last.started - a.last.started; });
  }
  // An agent's reply: fenced code as code blocks, `inline code` as code, the
  // rest as text (no HTML is ever interpreted).
  // Markdown, rendered safely: DOM nodes and text only (never innerHTML),
  // links only to http(s) addresses, no images (an agent's or a file's
  // Markdown must not fetch anything), raw HTML shown as text. Block parsing
  // is line by line and the inline patterns use negated classes, so the work
  // stays linear in the size of the text.
  var MD_INLINE = /`([^`\n]+)`|\*\*([^*\n]+)\*\*|__([^_\n]+)__|~~([^~\n]+)~~|!?\[([^\]\n]{0,300})\]\(([^()\s]{1,2000})\)|(https?:\/\/[^\s<>()\]]+[^\s<>()\].,;:!?'"])|\*([^*\n]+)\*|\b_([^_\n]+)_\b/g;
  function mdInline(text, el) {
    var t = String(text), last = 0, m;
    MD_INLINE.lastIndex = 0;
    while ((m = MD_INLINE.exec(t))) {
      if (m.index > last) el.appendChild(document.createTextNode(t.slice(last, m.index)));
      if (m[1] != null) el.appendChild(h("code", { text: m[1] }));
      else if (m[2] != null || m[3] != null) el.appendChild(h("strong", { text: m[2] != null ? m[2] : m[3] }));
      else if (m[4] != null) el.appendChild(h("del", { text: m[4] }));
      else if (m[6] != null) {
        var url = safeUrl(m[6]), label = m[5] || m[6];
        el.appendChild(url ? h("a", { href: url, target: "_blank", rel: "noopener noreferrer", text: label }) : document.createTextNode(label));
      } else if (m[7] != null) el.appendChild(h("a", { href: m[7], target: "_blank", rel: "noopener noreferrer", text: m[7] }));
      else el.appendChild(h("em", { text: m[8] != null ? m[8] : m[9] }));
      last = MD_INLINE.lastIndex;
    }
    if (last < t.length) el.appendChild(document.createTextNode(t.slice(last)));
    return el;
  }
  // A table's separator row (|---|:--:|): character classes only, linear.
  function mdIsSep(line) {
    var t = line.trim();
    return t.indexOf("|") >= 0 && t.indexOf("--") >= 0 && /^[|:\- \t]+$/.test(t);
  }
  function mdCells(line) {
    var x = line.trim().replace(/^\|/, "").replace(/\|$/, "");
    return x.split("|").map(function (c) { return c.trim(); });
  }
  function mdBody(text) {
    var out = h("div", { class: "body md" });
    var lines = String(text || "").replace(/\r\n?/g, "\n").split("\n"), i = 0, para = [];
    function flush() {
      if (!para.length) return;
      out.appendChild(mdInline(para.join("\n"), h("p")));
      para = [];
    }
    while (i < lines.length) {
      var l = lines[i], m;
      if ((m = /^\s*(```+|~~~+)\s*([\w+#.-]*)/.exec(l))) {
        flush();
        var fence = m[1], body = [];
        i++;
        while (i < lines.length && lines[i].trim().indexOf(fence) !== 0) { body.push(lines[i]); i++; }
        i++;
        out.appendChild(codeView(body.join("\n"), { lang: m[2], compact: true }));
        continue;
      }
      if (!l.trim()) { flush(); i++; continue; }
      if ((m = /^ {0,3}(#{1,6})[ \t]+(.*)$/.exec(l))) {
        flush();
        // Closing #s are dropped by hand: a regex for them backtracks badly.
        var ht = m[2].trim(), hj = ht.length;
        while (hj && ht.charAt(hj - 1) === "#") hj--;
        if (hj < ht.length && (hj === 0 || ht.charAt(hj - 1) === " ")) ht = ht.slice(0, hj).trim();
        out.appendChild(mdInline(ht, h("div", { class: "md-h md-h" + m[1].length, role: "heading", "aria-level": String(Math.min(6, m[1].length + 2)) })));
        i++; continue;
      }
      if (/^\s{0,3}([-*_])(\s*\1){2,}\s*$/.test(l)) { flush(); out.appendChild(h("hr")); i++; continue; }
      if (/^\s*>/.test(l)) {
        flush();
        var quote = [];
        while (i < lines.length && /^\s*>/.test(lines[i])) { quote.push(lines[i].replace(/^\s*>\s?/, "")); i++; }
        var bq = h("blockquote"); bq.appendChild(mdBody(quote.join("\n"))); out.appendChild(bq);
        continue;
      }
      if (/\|/.test(l) && i + 1 < lines.length && mdIsSep(lines[i + 1])) {
        flush();
        var head = mdCells(l), rows = [];
        i += 2;
        while (i < lines.length && /\|/.test(lines[i]) && lines[i].trim()) { rows.push(mdCells(lines[i])); i++; }
        out.appendChild(h("div", { class: "tablewrap" }, h("table", null,
          h("thead", null, h("tr", null, head.map(function (c) { return mdInline(c, h("th")); }))),
          h("tbody", null, rows.map(function (r) { return h("tr", null, head.map(function (_, k) { return mdInline(r[k] || "", h("td")); })); })))));
        continue;
      }
      if (/^\s*([-*+]|\d{1,9}[.)])\s+/.test(l)) {
        flush();
        // A list, nested by indentation.
        var stack = [];
        while (i < lines.length && lines[i].trim()) {
          var it = /^(\s*)([-*+]|\d{1,9}[.)])\s+(.*)$/.exec(lines[i]);
          if (!it && /^\s*(#{1,6}\s|>|```|~~~|\|)/.test(lines[i])) break;
          if (!it) {
            // A continuation line of the item above.
            var lastLi = stack.length ? stack[stack.length - 1].list.lastChild : null;
            if (lastLi) { lastLi.appendChild(document.createTextNode(" ")); mdInline(lines[i].trim(), lastLi); }
            i++; continue;
          }
          var ind = it[1].replace(/\t/g, "  ").length, ordered = /\d/.test(it[2]);
          while (stack.length && ind < stack[stack.length - 1].ind) stack.pop();
          // A bullet list followed by a numbered one (or back) at the same depth.
          if (stack.length && ind === stack[stack.length - 1].ind && (stack[stack.length - 1].list.tagName === "OL") !== ordered) {
            var was = stack.pop();
            if (!stack.length) { var fresh = h(ordered ? "ol" : "ul"); was.list.after(fresh); stack.push({ ind: ind, list: fresh }); }
          }
          if (!stack.length || ind > stack[stack.length - 1].ind) {
            var list = h(ordered ? "ol" : "ul");
            if (stack.length && stack[stack.length - 1].list.lastChild) stack[stack.length - 1].list.lastChild.appendChild(list);
            else out.appendChild(list);
            stack.push({ ind: ind, list: list });
          }
          var top = stack[stack.length - 1].list, li = h("li"), task = /^\[([ xX])\]\s+(.*)$/.exec(it[3]);
          if (task) { li.className = "task"; li.appendChild(h("span", { class: "tick", text: task[1] === " " ? "○ " : "✓ " })); mdInline(task[2], li); }
          else mdInline(it[3], li);
          top.appendChild(li);
          i++;
        }
        continue;
      }
      para.push(l.trim());
      i++;
    }
    flush();
    return out;
  }
  // Code with line numbers; a diff gets its lines coloured and numbered on
  // both sides.
  function codeView(text, o) {
    o = o || {};
    var lines = String(text).split("\n");
    if (lines.length > 1 && lines[lines.length - 1] === "") lines.pop();
    var tbl = h("table", { class: "code-view" + (o.compact ? " compact" : "") });
    var tb = h("tbody"); tbl.appendChild(tb);
    if (o.diff) {
      var a = 0, b = 0;
      lines.forEach(function (l) {
        var m = /^@@ -(\d+)(?:,\d+)? \+(\d+)/.exec(l), cls = "", la = "", lb = "";
        if (m) { a = +m[1]; b = +m[2]; cls = "hunk"; }
        else if (/^(diff |index |--- |\+\+\+ |new file|deleted file)/.test(l)) cls = "meta";
        else if (l.charAt(0) === "+") { cls = "add"; lb = b++; }
        else if (l.charAt(0) === "-") { cls = "del"; la = a++; }
        else { la = a++; lb = b++; }
        tb.appendChild(h("tr", { class: cls }, h("td", { class: "ln", text: String(la) }), h("td", { class: "ln", text: String(lb) }), h("td", { class: "src", text: l || " " })));
      });
    } else {
      lines.forEach(function (l, i) { tb.appendChild(h("tr", null, h("td", { class: "ln", text: String(i + 1) }), h("td", { class: "src", text: l || " " }))); });
    }
    return h("div", { class: "code-wrap" }, o.lang ? h("div", { class: "code-lang", text: o.lang }) : null, tbl);
  }
  // A wide dialog for a diff or a file.
  // A Markdown file: rendered, with a switch to its source.
  function mdOrSource(text) {
    var box = h("div"), shown = mdBody(text), src = null;
    var sw = h("button", { type: "button", class: "ghost small", onclick: function () {
      if (!src) src = codeView(text, {});
      var showSrc = sw.textContent === "Source";
      box.replaceChild(showSrc ? src : shown, showSrc ? shown : src);
      sw.textContent = showSrc ? "Rendered" : "Source";
    } }, "Source");
    box.appendChild(h("div", { class: "row" }, sw));
    box.appendChild(shown);
    return box;
  }
  function codeDialog(title, body) {
    var wrap = h("label", { class: "check small" }, h("input", { type: "checkbox", onchange: function (e) { dlg.classList.toggle("wrap", e.target.checked); } }), " Wrap lines");
    var dlg = h("dialog", { class: "ask wide code", "aria-label": title },
      h("div", { class: "row" }, h("h3", { class: "code-title", text: title }), wrap, h("button", { type: "button", onclick: function () { dlg.close(); dlg.remove(); } }, "Close")), body);
    dlg.addEventListener("cancel", function () { dlg.remove(); });
    document.body.appendChild(dlg); dlg.showModal();
  }

  function firstLine(t, n) { t = String(t || "").split("\n").filter(function (x) { return x.trim(); })[0] || "(no prompt)"; return t.length > n ? t.slice(0, n) + "..." : t; }
  var DOT = { running: "run live", exited: "ok", failed: "bad", stopped: "warn" };

  function pageSessions(m, arg) {
    var ro = S.ov.read_only;
    var wrap = h("div", { class: "sessions" });
    var side = h("aside", { class: "s-side" }), mid = h("section", { class: "s-chat", "aria-live": "polite" }), panel = h("aside", { class: "s-panel" });
    wrap.appendChild(side); wrap.appendChild(mid); wrap.appendChild(panel);
    m.appendChild(wrap);
    var search = h("input", { type: "search", placeholder: "Search sessions", "aria-label": "Search sessions" });
    var listBox = h("div", { class: "s-list", role: "navigation", "aria-label": "Sessions" });
    side.appendChild(h("div", { class: "row" }, h("a", { class: "btn primary", href: "#/launch", text: "+ New" }), h("a", { class: "small", href: "#/procs", text: "Table view" })));
    side.appendChild(search); side.appendChild(listBox);
    var chains = [], selected = arg ? Number(arg) : null, streams = [], shownKey = null;
    search.addEventListener("input", drawList);
    S.cleanup.push(function () { streams.forEach(function (x) { x.close(); }); });

    function drawList() {
      var qy = search.value.trim().toLowerCase();
      listBox.textContent = "";
      var shown = chains.filter(function (c) { return !qy || c.procs.some(function (p) { return (p.prompt + " " + p.cwd + " " + p.node).toLowerCase().indexOf(qy) >= 0; }); });
      if (!shown.length) listBox.appendChild(h("p", { class: "muted small", text: chains.length ? "No match." : "No sessions yet. Start one with + New." }));
      shown.forEach(function (c) {
        var on = c.procs.some(function (p) { return p.id === selected; });
        listBox.appendChild(h("a", { class: "s-item" + (on ? " on" : ""), href: "#/sessions/" + c.last.id },
          h("span", { class: "dot " + (DOT[c.last.status] || ""), title: c.last.status }),
          h("span", { class: "s-title", text: firstLine(c.root.prompt, 70) }),
          h("span", { class: "muted small", text: c.last.node + "  -  " + ago(c.last.started) + (c.last.pending && c.last.pending.length ? "  -  needs you" : "") })));
      });
    }
    function chainOf(id) { return chains.find(function (c) { return c.procs.some(function (p) { return p.id === id; }); }); }

    // The conversation: each process's prompt, then its events; tool steps fold.
    var evs = {};
    function drawChat() {
      var c = chainOf(selected);
      mid.textContent = "";
      if (!c) { mid.appendChild(empty(chains.length ? "Pick a session on the left." : "No sessions yet: + New starts an agent.")); return; }
      var atBottom = true;
      var log = h("div", { class: "s-log" });
      c.procs.forEach(function (p, i) {
        log.appendChild(h("div", { class: "msg user" }, h("div", { class: "who", text: i === 0 ? "You" : "You (continued)" }), h("div", { class: "body", text: String(p.prompt || "").slice(0, 4000) })));
        var fold = null;
        function flush() { if (fold) { log.appendChild(fold.el); fold = null; } }
        (evs[p.id] || []).forEach(function (e) {
          if (e.kind === "user") { flush(); log.appendChild(h("div", { class: "msg user" }, h("div", { class: "who", text: "You" }), h("div", { class: "body", text: e.text }))); return; }
          if (e.kind === "text" && !e.agent) { flush(); log.appendChild(h("div", { class: "msg agent" }, mdBody(e.text))); return; }
          if (e.kind === "result") { flush(); return; }
          if (e.kind === "exit") { flush(); log.appendChild(h("div", { class: "msg status " + (/^exited/.test(e.text) ? "ok" : "bad"), text: e.text })); return; }
          if (!fold) {
            var items = h("ul", { class: "steps-list" });
            fold = { n: 0, items: items, sum: h("summary"), el: null };
            fold.el = h("details", { class: "fold", keep: "fold:" + p.id + ":" + e.seq }, fold.sum, items);
          }
          fold.n++;
          fold.items.appendChild(h("li", { class: e.kind }, e.agent ? h("span", { class: "pill", text: e.agent }) : null, " ", e.text));
          fold.sum.textContent = fold.n === 1 ? e.text.slice(0, 120) : fold.n + " steps: " + e.text.slice(0, 90);
        });
        flush();
      });
      var last = c.last;
      if ((last.pending || []).length && !ro) {
        last.pending.forEach(function (a) {
          log.appendChild(h("div", { class: "msg ask" }, h("b", { text: "It asks to use " + a.tool }),
            h("pre", { class: "view small", text: JSON.stringify(a.input, null, 2).slice(0, 2000) }),
            h("div", { class: "row" },
              h("button", { type: "button", class: "primary", onclick: function () { answer(last.id, a.request_id, true); } }, "Allow"),
              h("button", { type: "button", onclick: function () { answer(last.id, a.request_id, false); } }, "Deny"))));
        });
      }
      if (last.status === "running") log.appendChild(h("div", { class: "msg status run", text: (last.pending || []).length ? "Waiting for you" : "Working..." }));
      mid.appendChild(log);
      // The box: a follow-up while it runs (queued for its next turn), or a
      // continuation of the session once it ended.
      if (!ro) {
        var box = h("textarea", { rows: "2", placeholder: last.open ? "Reply (sent now, or queued until its current turn ends)" : last.can_resume && last.session ? "Continue this session" : "This agent cannot be continued (no session); start a new one" });
        var go = h("button", { type: "submit", class: "primary" }, last.open ? "Send" : "Continue");
        var stop = last.status === "running" ? h("button", { type: "button", class: "ghost", onclick: function () {
          if (!window.confirm("Stop this agent?")) return;
          api("/api/procs/" + last.id + "/stop", { method: "POST" }).then(refresh).catch(function (e) { toast(e.message, true); });
        } }, "Stop") : null;
        var form = h("form", { class: "s-input", "data-nodirty": "", onsubmit: function (e) {
          e.preventDefault(); var t = box.value.trim(); if (!t) return; go.disabled = true;
          var call = last.open ? api("/api/procs/" + last.id + "/message", { method: "POST", body: { text: t } })
            : api("/api/procs/" + last.id + "/pivot", { method: "POST", body: { prompt: t, mode: "continue" } });
          call.then(function (r) { box.value = ""; go.disabled = false; if (r && r.id) location.hash = "#/sessions/" + r.id; else refresh(); })
            .catch(function (err) { toast(err.message, true); go.disabled = false; });
        } }, box, h("div", { class: "row" }, go, stop, h("a", { class: "small", href: "#/proc/" + last.id, text: "Details" })));
        box.addEventListener("keydown", function (e) { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); form.requestSubmit(); } });
        mid.appendChild(form);
      }
      if (atBottom) log.scrollTop = log.scrollHeight;
    }
    function answer(id, rid, allow) {
      api("/api/procs/" + id + "/permission", { method: "POST", body: { request_id: rid, allow: allow, always: false } }).then(refresh).catch(function (e) { toast(e.message, true); });
    }

    // The side panel: where it works and what it did.
    var wsFor = null, ws = null;
    function drawPanel() {
      var c = chainOf(selected); panel.textContent = "";
      if (!c) return;
      var p = c.last;
      function sec(title) { var b = h("div", { class: "s-sec" }, h("h3", { text: title })); panel.appendChild(b); return b; }
      var env = sec("Environment");
      env.appendChild(h("div", { class: "kv" }, h("span", { text: "Machine" }), h("b", { text: p.node })));
      env.appendChild(h("div", { class: "kv" }, h("span", { text: "Folder" }), h("code", { class: "small", text: p.cwd })));
      if (ws && ws.git) env.appendChild(h("div", { class: "kv" }, h("span", { text: "Branch" }), h("code", { text: ws.branch })));
      env.appendChild(h("div", { class: "kv" }, h("span", { text: "Agent" }), h("span", { text: p.runner + (p.model ? ", " + p.model : "") + (p.mode ? ", " + p.mode : "") })));
      if (p.context_tokens && p.context_window) {
        var pct = Math.min(100, Math.round(100 * p.context_tokens / p.context_window));
        var fill = h("i"); fill.style.width = pct + "%";
        var cx = sec("Context");
        cx.appendChild(h("div", { class: "row" }, h("div", { class: "meter" + (pct > 80 ? " low" : "") }, fill), h("span", { class: "small", text: pct + "% of " + Math.round(p.context_window / 1000) + "k" })));
        if (p.base_tokens) {
          var servers = (p.mcp || []).length, kb = Math.round(p.base_tokens / 1000);
          cx.appendChild(h("p", { class: "muted small", text: kb + "k of it was there at its first reply: Claude Code's own instructions and tools" +
            (servers ? ", the tools of " + servers + " MCP server" + (servers === 1 ? "" : "s") : "") + ", the skills and subagents it can call, CLAUDE.md files and the prompt." +
            (p.base_tokens > 30000 && servers ? " To start smaller, run it with MCP servers: none (Run an agent > More options, or in its profile)." : "") }));
        }
        if (pct > 60) cx.appendChild(h("p", { class: "muted small", text: "Every turn re-reads this context. For an unrelated task, start a new session: it costs less." }));
      }
      // Tokens over the whole session (all its continuations).
      var tk = { input: 0, output: 0, cache_read: 0, cache_write: 0 }, cost = 0, any = false;
      c.procs.forEach(function (x) {
        if (x.tokens) { any = true; Object.keys(tk).forEach(function (k) { tk[k] += x.tokens[k] || 0; }); }
        if (x.cost_usd) cost += x.cost_usd;
      });
      if (any) {
        var read = tk.input + tk.cache_read + tk.cache_write, kfmt = function (n) { return n >= 1000 ? (n / 1000).toFixed(n >= 100000 ? 0 : 1) + "k" : String(n); };
        var u = sec("Usage");
        u.appendChild(h("div", { class: "kv" }, h("span", { text: "Input" }), h("span", { text: kfmt(read) + (read ? ", " + Math.round(100 * tk.cache_read / read) + "% from cache" : "") })));
        u.appendChild(h("div", { class: "kv" }, h("span", { text: "Output" }), h("span", { text: kfmt(tk.output) })));
        if (cost) u.appendChild(h("div", { class: "kv" }, h("span", { text: "Cost" }), h("span", { text: "$" + cost.toFixed(cost < 1 ? 4 : 2) })));
      }
      var todos = c.procs.slice().reverse().map(function (x) { return x.todos; }).find(function (t) { return Array.isArray(t) && t.length; });
      if (todos) {
        var tl = sec("Tasks").appendChild(h("ul", { class: "todos" }));
        todos.forEach(function (t) { tl.appendChild(h("li", { class: t.status || "" }, h("span", { class: "tick", text: t.status === "completed" ? "\u2713" : t.status === "in_progress" ? "\u25cb" : "\u00b7" }), " ", t.content || t.activeForm || "")); });
      }
      var files = {}; c.procs.forEach(function (x) { (x.files || []).forEach(function (f) { files[f] = 1; }); });
      var changed = ws && ws.changed ? ws.changed : null;
      var ch = sec("Changes");
      if (changed && changed.length) {
        changed.slice(0, 100).forEach(function (f) {
          ch.appendChild(h("button", { type: "button", class: "ghost file", title: "Show the diff", onclick: function () { showDiff(p.id, f.path); } }, h("span", { class: "fstat", text: f.status }), " ", f.path));
        });
      } else if (Object.keys(files).length) {
        Object.keys(files).forEach(function (f) { ch.appendChild(h("div", { class: "small cmd", text: f })); });
        if (ws && ws.local === false) ch.appendChild(h("p", { class: "muted small", text: "Diffs show for agents on this machine." }));
      } else ch.appendChild(h("p", { class: "muted small", text: ws && ws.git ? "No changes in its folder." : "None yet." }));
      if (S.ov.role !== "viewer" && !(p.archived && p.node !== "local")) panel.appendChild(filesSection(p));
      var links = {}; c.procs.forEach(function (x) { (x.links || []).forEach(function (u) { links[u] = 1; }); });
      if (Object.keys(links).length) {
        var lk = sec("Pull requests and issues");
        Object.keys(links).forEach(function (u) { var sh = u.replace("https://github.com/", ""); lk.appendChild(h("a", { class: "small block", href: safeUrl(u), target: "_blank", rel: "noopener noreferrer", text: sh })); });
      }
    }
    function showDiff(id, path) {
      api("/api/procs/" + id + "/diff" + q({ path: path })).then(function (d) {
        codeDialog(path + " (changes)", h("div", null, d.diff ? codeView(d.diff, { diff: true }) : h("p", { class: "muted", text: "No difference." }),
          d.truncated ? h("p", { class: "muted small", text: "Shortened: the diff is large." }) : null,
          h("button", { type: "button", class: "ghost small", onclick: function () { showFile(id, path); } }, "Open the whole file")));
      }).catch(function (e) { toast(e.message, true); });
    }
    function showFile(id, path) {
      api("/api/procs/" + id + "/file" + q({ path: path })).then(function (f) {
        var body = f.binary ? h("p", { class: "muted", text: "A binary file (" + bytes(f.size) + "): not shown." })
          : f.too_big ? h("p", { class: "muted", text: "Larger than 1 MB (" + bytes(f.size) + "): not shown." })
          : /\.(md|markdown)$/i.test(path) ? mdOrSource(f.text) : codeView(f.text, {});
        codeDialog(path, body);
      }).catch(function (e) { toast(e.message, true); });
    }
    // Files where it works: a folder browser, one level at a time.
    var browse = "";
    function filesSection(p) {
      var box = h("div", { class: "s-sec" }, h("h3", { text: "Files" }));
      var crumbs = h("div", { class: "crumbs small" }), list = h("div", { class: "flist" });
      box.appendChild(crumbs); box.appendChild(list);
      function go(path) {
        browse = path;
        crumbs.textContent = "";
        var parts = path ? path.split("/") : [];
        crumbs.appendChild(h("button", { type: "button", class: "ghost small", onclick: function () { go(""); } }, (p.cwd.split(/[\\/]/).filter(Boolean).pop() || "folder")));
        parts.forEach(function (x, i) {
          crumbs.appendChild(document.createTextNode(" / "));
          crumbs.appendChild(h("button", { type: "button", class: "ghost small", onclick: function () { go(parts.slice(0, i + 1).join("/")); } }, x));
        });
        list.textContent = "";
        list.appendChild(h("p", { class: "muted small", text: "Loading..." }));
        api("/api/procs/" + p.id + "/files" + q({ path: path })).then(function (r) {
          list.textContent = "";
          if (!r.entries.length) list.appendChild(h("p", { class: "muted small", text: "Empty." }));
          r.entries.forEach(function (e) {
            var full = (path ? path + "/" : "") + e.name;
            list.appendChild(h("button", { type: "button", class: "ghost file", title: e.dir ? "Open the folder" : bytes(e.size), onclick: function () { if (e.dir) go(full); else showFile(p.id, full); } },
              h("span", { class: "ficon", text: e.dir ? "\u25b8" : "\u00b7" }), " ", e.name));
          });
          if (r.more) list.appendChild(h("p", { class: "muted small", text: "Showing the first 1000." }));
        }).catch(function (e) { list.textContent = ""; list.appendChild(h("p", { class: "muted small", text: e.message })); });
      }
      go(browse);
      return box;
    }

    // Live: one stream per process of the selected session.
    function follow(c) {
      var key = c.procs.map(function (p) { return p.id; }).join(",");
      if (key === shownKey) return;
      shownKey = key;
      streams.forEach(function (x) { x.close(); }); streams = [];
      c.procs.forEach(function (p) {
        evs[p.id] = [];
        var es = new EventSource("/api/procs/" + p.id + "/events");
        var t = null;
        es.onmessage = function (m) { try { evs[p.id].push(JSON.parse(m.data)); } catch (x) { /* skip */ } clearTimeout(t); t = setTimeout(drawChat, 120); };
        streams.push(es);
      });
    }
    function refresh() {
      return api("/api/procs").then(function (procs) {
        chains = sessionChains(procs);
        if (!selected && chains.length) selected = chains[0].last.id;
        var c = chainOf(selected);
        if (c && c.last.id !== selected && arg == null) selected = c.last.id;
        drawList();
        if (c) {
          follow(c);
          if (wsFor !== c.last.id || c.last.status === "running") {
            wsFor = c.last.id;
            api("/api/procs/" + c.last.id + "/workspace").then(function (w) { ws = w; drawPanel(); }).catch(function () { ws = null; drawPanel(); });
          }
        }
        drawChat(); drawPanel();
      }).catch(function (e) { mid.textContent = e.message; });
    }
    refresh(); every(3000, function () { if (!document.activeElement || !mid.contains(document.activeElement)) refresh(); else api("/api/procs").then(function (p) { chains = sessionChains(p); drawList(); drawPanel(); }); });
  }

  function pageProcs(m) {
    m.appendChild(header("Running", "Agents started from this console: live ones first. Open one to watch it, answer its questions or steer it. Logs stay under " + S.ov.data_dir + "."));
    var plans = h("div"); m.appendChild(plans);
    var holder = h("div"); m.appendChild(holder);
    function load() {
      api("/api/delegations").then(function (list) {
        plans.textContent = "";
        if (!list.length) return;
        plans.appendChild(h("h2", { text: "Plans" }));
        plans.appendChild(h("div", { class: "tablewrap" }, h("table", null,
          h("thead", null, h("tr", null, ["#", "Status", "Task", "Steps", "Roles", "Cost"].map(function (t) { return h("th", { text: t }); }))),
          h("tbody", null, list.map(function (r) {
            var d = r.delegation, go = function () { location.hash = "#/plan/" + d.id; };
            return h("tr", { class: "click", tabindex: "0", onclick: go, onkeydown: function (e) { if (e.key === "Enter") go(); } },
              h("td", { text: String(d.id) }), h("td", null, h("span", { class: "pill " + (PLAN_STATE[d.status] || ""), text: d.status })),
              h("td", { class: "desc", text: d.task.slice(0, 120) }), h("td", { text: d.plan ? String(d.plan.steps.length) : "-" }),
              h("td", { class: "muted", text: d.roles.planner + " > " + d.roles.worker }),
              h("td", { text: r.total_cost_usd ? "$" + r.total_cost_usd.toFixed(4) : "-" }));
          })))));
        plans.appendChild(h("h2", { text: "Agents" }));
      }).catch(function () { /* plans are optional */ });
      api("/api/procs").then(function (list) {
        holder.textContent = "";
        holder.appendChild(list.length ? procTable(list) : empty("Nothing launched yet."));
      });
    }
    load(); every(2000, load);
  }

  function termUrl(params) { return "/term?" + new URLSearchParams(params).toString(); }

  // A link that opens a terminal: as a tab in the terminals page when one is
  // open already (it answers on a BroadcastChannel), else in a new tab.
  var termBus = window.BroadcastChannel ? new BroadcastChannel("indra-term") : null, termPings = {}, termPingN = 0;
  if (termBus) termBus.onmessage = function (e) { var m = e.data || {}; if (m.t === "pong" && termPings[m.n]) termPings[m.n](); };
  function termLink(label, params, cls) {
    var a = h("a", { class: cls || "btn", href: termUrl(params), target: "_blank", rel: "noopener" }, label);
    a.termParams = params;
    a.addEventListener("click", function (e) {
      if (!termBus || e.ctrlKey || e.metaKey || e.shiftKey || e.button !== 0) return;
      e.preventDefault();
      var n = ++termPingN, answered = false;
      termPings[n] = function () {
        if (answered) return;
        answered = true; delete termPings[n];
        termBus.postMessage({ t: "open", params: a.termParams });
        toast("Opened in your terminals tab");
      };
      termBus.postMessage({ t: "ping", n: n });
      setTimeout(function () { if (!answered) { answered = true; delete termPings[n]; window.open(a.href, "_blank", "noopener"); } }, 250);
    });
    return a;
  }

  // The terminals running on this machine, with Close.
  function runningTerminals(box) {
    function draw() {
      return api("/api/term/sessions").then(function (list) {
        box.textContent = "";
        if (!list.length) { box.appendChild(empty("No terminals open.")); return; }
        box.appendChild(h("div", { class: "rowlist" }, list.map(function (s) {
          return h("div", { class: "rowitem" }, h("span", { class: "pill ok", text: "#" + s.id }),
            h("span", { class: "grow clip1", title: s.cwd, text: s.label }),
            h("span", { class: "muted small", text: (s.user ? s.user + ", " : "") + ago(s.started) }),
            h("button", { type: "button", class: "danger", onclick: function (e) {
              if (!window.confirm("Close " + s.label + "? Its program, and anything it started, ends.")) return;
              e.target.disabled = true;
              api("/api/term/sessions/" + s.id + "/close", { method: "POST" }).then(function () {
                if (termBus) termBus.postMessage({ t: "closed", id: s.id });
                toast("Closed terminal #" + s.id);
                setTimeout(draw, 300);
              }).catch(function (x) { e.target.disabled = false; toast(x.message, true); });
            } }, "Close"));
        })));
      }).catch(function (x) { box.textContent = ""; box.appendChild(empty(x.message)); });
    }
    draw(); every(3000, draw);
  }

  // Terminal: a real shell in the browser (when `indra serve --shell`), and
  // the terminal client for the user's own terminal.
  function pageTerminal(m) {
    m.appendChild(header("Terminal", "Look around and steer agents from a terminal: in this browser, or in your own."));
    var page = h("div", { class: "stack" }); m.appendChild(page); m = page;
    var own = h("div", { class: "card" },
      h("h3", { text: "In your own terminal" }),
      h("p", { class: "muted", text: "indra attach follows an agent live and steers it: approve tool requests, change its mode or model, send messages, pivot. It finds this console by itself on this machine; elsewhere pass --url and --token." }),
      h("div", { class: "cmd lines", text: "indra agents            # what runs, and what waits for you\nindra attach 12         # follow process 12; /help lists the commands" }));
    if (!S.ov.shell) {
      m.appendChild(h("div", { class: "card" },
        h("h3", { text: "Browser terminal is off" }),
        h("p", { class: "muted", text: "It is a shell on this machine, so it is opt-in: start the console with --shell. It is offered only on a loopback address and never in read-only mode, and each session is logged to logs/terminal.log." }),
        h("div", { class: "cmd", text: "indra serve --shell" })));
      m.appendChild(own);
      return;
    }
    var hs = harness();
    var targets = hs ? (hs.targets.length ? hs.targets : [hs.root]) : [];
    var hasClaude = (S.ov.runners || []).some(function (r) { return r.format === "claude-stream-json" && r.available; });
    var claudeProfiles = (S.ov.profiles || []).filter(function (p) { return !p.runner || p.runner === "claude"; });
    var prof = h("select", { "aria-label": "Claude Code profile" }, h("option", { value: "", text: "no profile (your own Claude Code setup)" }),
      claudeProfiles.map(function (p) { return h("option", { value: p.name, text: p.name + (p.provider === "ollama" ? "  (on Ollama: " + (p.model || "?") + ")" : p.subagent_provider === "ollama" ? "  (subagents on Ollama)" : "") }); }));
    var claudeLinks = [], shellLinks = [];
    var shells = S.ov.shells || [];
    var shellSel = h("select", { "aria-label": "Shell" }, shells.map(function (p, i) { return h("option", { value: i ? p : "", text: p.split(/[\\/]/).pop() + (i ? "" : " (default)") }); }));
    shellSel.addEventListener("change", function () { shellLinks.forEach(function (x) { x.a.termParams = shellAt(x.at); x.a.href = termUrl(x.a.termParams); }); });
    function shellAt(at) { return Object.assign({ what: "shell" }, at, shellSel.value ? { shell: shellSel.value } : {}); }
    function claudeAt(at) { return Object.assign({ what: "claude" }, at, prof.value ? { profile: prof.value } : {}); }
    prof.addEventListener("change", function () { claudeLinks.forEach(function (x) { x.a.termParams = claudeAt(x.at); x.a.href = termUrl(x.a.termParams); }); });
    m.appendChild(h("div", { class: "card" },
      h("h3", { text: "Start a terminal" }),
      h("p", { class: "muted", text: "Opens in the terminals tab (one tab, a tab inside it per terminal), in the directory you pick. Close a terminal with its \u00d7, or below." }),
      hasClaude && claudeProfiles.length ? h("div", { class: "row termprof" }, h("span", { class: "muted small", text: "Claude Code with" }), prof,
        h("span", { class: "muted small", text: "A profile on Ollama runs Claude Code on that local model; see the local models guide." })) : null,
      shells.length > 1 ? h("div", { class: "row termprof" }, h("span", { class: "muted small", text: "Shell" }), shellSel,
        h("span", { class: "muted small" }, "The default and its arguments are in ", h("a", { href: "#/settings/general", text: "Settings > General" }), ".")) : null,
      targets.length ? h("div", { class: "rowlist" }, targets.map(function (t, i) {
        var at = hs.targets.length ? { h: hs.name, target: i } : { h: hs.name };
        var ca = hasClaude ? termLink("Claude Code", claudeAt(at)) : null;
        if (ca) claudeLinks.push({ a: ca, at: at });
        return h("div", { class: "rowitem" }, h("code", { class: "grow", text: t }), h("span", { class: "row" },
          (function () { var sl = termLink("Shell", shellAt(at)); shellLinks.push({ a: sl, at: at }); return sl; })(), ca));
      })) : empty("No harness, so no directory to start in.")));
    var running = h("div");
    m.appendChild(h("div", { class: "card" },
      h("h3", { text: "Running terminals" }),
      h("p", { class: "muted", text: "Every terminal open on this machine, in any browser. Closing one ends its program and anything it started." }),
      h("div", { class: "row" }, h("a", { class: "btn ghost", href: "/term", target: "_blank", rel: "noopener" }, "Open the terminals tab")),
      running));
    runningTerminals(running);
    var sessions = h("div");
    m.appendChild(h("div", { class: "card" },
      h("h3", { text: "Agent sessions" }),
      h("p", { class: "muted", text: "Open an agent's own Claude Code session interactively, in its directory. While the agent still runs, the terminal gets a branch of the session (--fork-session), so the agent carries on undisturbed." }),
      sessions));
    m.appendChild(own);
    api("/api/procs").then(function (list) {
      var ok = list.filter(function (p) { return p.node === "local" && p.session; });
      sessions.appendChild(ok.length ? h("div", { class: "rowlist" }, ok.map(function (p) {
        return h("div", { class: "rowitem" }, h("a", { href: "#/proc/" + p.id, text: "#" + p.id }), statusPill(p.status),
          h("span", { class: "muted grow clip1", text: p.prompt || "" }),
          termLink(p.status === "running" ? "Fork in terminal" : "Open in terminal", { what: "agent", proc: p.id }));
      })) : empty("No local agent has reported a session yet (Claude Code does)."));
    }).catch(function (e) { sessions.appendChild(empty(e.message)); });
  }

  function pageProc(m, id) {
    var top = h("div"), agentsBox = h("div"), toolsBox = h("div"), feed = h("div", { class: "feed", role: "log", "aria-live": "polite" });
    var follow = h("input", { type: "checkbox", checked: true, id: "follow" });
    // Pivot: give this agent a new direction.
    var pv = {
      text: h("textarea", { "aria-label": "New direction", placeholder: "e.g. Stop the SQLi hunt and focus on the JWT validation in auth/" }),
      cont: h("input", { type: "radio", name: "pivot-mode", value: "continue", checked: true }),
      fresh: h("input", { type: "radio", name: "pivot-mode", value: "fresh" }),
      keep: h("input", { type: "checkbox" }),
      go: h("button", { class: "primary", type: "submit" }, "Pivot"),
      note: h("p", { class: "muted small" }),
      // A fresh start may switch profile (and so runner) and model.
      profile: h("select", { "aria-label": "Pivot profile" }, h("option", { value: "", text: "the same profile" }),
        (S.ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name + (p.provider === "ollama" ? "  (Ollama: " + (p.model || "?") + ")" : p.model ? "  (" + p.model + ")" : "") }); })),
      model: h("input", { type: "text", list: "models", "aria-label": "Pivot model", placeholder: "the same model" })
    };
    var switchRow = h("div", { class: "fields pivotswitch" }, h("label", null, "Profile (fresh start)", pv.profile), h("label", null, "Model", pv.model));
    function pivotMode() { pv.profile.disabled = !pv.fresh.checked; if (!pv.fresh.checked) pv.profile.value = ""; }
    [pv.cont, pv.fresh].forEach(function (r) { r.addEventListener("change", pivotMode); });
    pivotMode();
    [pv.cont, pv.fresh].forEach(function (r) { r.addEventListener("change", function () { pv.chosen = true; }); });
    var keepRow = h("label", { class: "check" }, pv.keep, "Keep this agent running (the continuation branches its session)");
    var pivotCard = h("form", { class: "card pivot", onsubmit: function (e) {
      e.preventDefault();
      if (!pv.text.value.trim()) { toast("Write the new direction first", true); return; }
      pv.go.disabled = true;
      var pbody = { prompt: pv.text.value, mode: pv.fresh.checked ? "fresh" : "continue", keep_running: pv.keep.checked };
      if (pv.fresh.checked && pv.profile.value) pbody.profile = pv.profile.value;
      if (pv.model.value.trim()) pbody.opts = { model: pv.model.value.trim() };
      api("/api/procs/" + id + "/pivot", { method: "POST", body: pbody })
        .then(function (r) { location.hash = "#/proc/" + r.id; })
        .catch(function (err) { toast(err.message, true); pv.go.disabled = false; });
    } },
      h("h3", { text: "Pivot this agent" }),
      pv.text,
      h("label", { class: "check" }, pv.cont, h("span", null, h("b", { text: "Continue its session" }), " - keeps everything it learned")),
      h("label", { class: "check" }, pv.fresh, h("span", null, h("b", { text: "Start fresh" }), " - a new agent, given this one's question and result")),
      switchRow, keepRow, pv.note, h("div", { class: "row" }, pv.go), modelList());
    // Waiting tool requests, and live control of an interactive agent.
    var approvals = h("div", { class: "card approvals", hidden: true, role: "region", "aria-label": "Tool requests waiting for you" });
    var ctxCard = h("div", { class: "card context" });
    var lf = { title: h("input", { type: "text", placeholder: "A short title", "aria-label": "Learning title" }),
      body: h("textarea", { rows: "3", placeholder: "What should a future agent know?", "aria-label": "Learning" }) };
    var learnForm = h("form", { class: "form", hidden: true, onsubmit: function (e) {
      e.preventDefault();
      api("/api/learnings", { method: "POST", body: { title: lf.title.value, body: lf.body.value, proc: Number(id) } })
        .then(function () { toast("Added to Knowledge > Learnings"); lf.title.value = ""; lf.body.value = ""; learnForm.hidden = true; refreshOverview(); })
        .catch(function (err) { toast(err.message, true); });
    } }, lf.title, lf.body, h("div", null, h("button", { type: "submit" }, "Add to the inbox")));
    function renderContext(p) {
      ctxCard.textContent = "";
      ctxCard.appendChild(h("h3", { text: "Context" }));
      if (p.context_tokens) {
        var win = p.context_window || 200000, pct = Math.min(100, Math.round(100 * p.context_tokens / win));
        var fill = h("div", { class: "fill" + (pct >= 80 ? " hot" : "") }); fill.style.width = pct + "%";
        ctxCard.appendChild(h("div", { class: "bar", title: p.context_tokens + " tokens in context at the last turn" }, h("span", { text: "window" }), h("div", { class: "track" }, fill),
          h("b", { text: Math.round(p.context_tokens / 1000) + "k / " + Math.round(win / 1000) + "k" })));
      } else {
        ctxCard.appendChild(h("p", { class: "muted small", text: p.interactive || p.runner === "claude" ? "Context size appears after its first turn." : "This runner does not report its context size." }));
      }
      ctxCard.appendChild(h("p", { class: "small" }, h("b", { text: "Memory: " }), p.memory.length ? null : h("span", { class: "muted", text: "none given" })));
      if (p.memory.length) ctxCard.appendChild(h("ul", { class: "links" }, p.memory.map(function (n) {
        return h("li", null, h("a", { href: "#/knowledge/notes/" + n.vault + "/" + encodeURIComponent(n.id), text: n.title }));
      })));
      var rf = !p.reflect ? null : p.reflection === "asked" ? "Asked for learnings; waiting for its answer." : p.reflection === "received" ? "Its learnings are in Knowledge > Learnings." : "Will be asked for learnings before it finishes.";
      if (rf) ctxCard.appendChild(h("p", { class: "small" }, h("b", { text: "Learnings: " }), rf));
      if (!S.ov.read_only) {
        ctxCard.appendChild(h("button", { type: "button", class: "ghost", onclick: function () { learnForm.hidden = !learnForm.hidden; if (!learnForm.hidden) lf.title.focus(); } }, "Save a learning from this agent"));
        ctxCard.appendChild(learnForm);
      }
    }
    var cv = {
      mode: h("select", { "aria-label": "Permission mode" }, MODES.map(function (x) { return h("option", { value: x[0], text: x[1] }); })),
      model: h("input", { type: "text", list: "models", "aria-label": "Model", placeholder: "opus, sonnet, a full model id..." }),
      msg: h("textarea", { "aria-label": "Message", placeholder: "A follow-up for this agent, in the same session" }),
      state: h("p", { class: "muted small" }), note: h("p", { class: "muted small" })
    };
    function ctl(body, done) {
      return api("/api/procs/" + id + "/control", { method: "POST", body: body }).then(function () { toast(done); refresh(); }).catch(function (e) { toast(e.message, true); });
    }
    var controlCard = h("div", { class: "card control", hidden: true },
      h("h3", { text: "Control" }), cv.state,
      h("div", { class: "ctlrow" }, cv.mode, h("button", { type: "button", onclick: function () { ctl({ mode: cv.mode.value }, "Mode change sent"); } }, "Set mode")),
      h("div", { class: "ctlrow" }, cv.model, h("button", { type: "button", onclick: function () {
        if (!cv.model.value.trim()) { toast("Type a model first", true); return; }
        ctl({ model: cv.model.value.trim() }, "Model change sent");
      } }, "Set model")),
      h("div", { class: "row" }, h("button", { type: "button", class: "danger", onclick: function () { ctl({ interrupt: true }, "Interrupt sent"); } }, "Interrupt current turn")),
      cv.msg,
      h("div", { class: "row" }, h("button", { type: "button", class: "primary", onclick: function () {
        if (!cv.msg.value.trim()) return;
        api("/api/procs/" + id + "/message", { method: "POST", body: { text: cv.msg.value } })
          .then(function () { cv.msg.value = ""; toast("Sent"); refresh(); }).catch(function (e) { toast(e.message, true); });
      } }, "Send message")), cv.note, modelList());
    var ctlFilled = false;
    function renderControl(p) {
      var ro = S.ov.read_only;
      approvals.textContent = "";
      approvals.hidden = !p.pending.length;
      if (p.pending.length) {
        approvals.appendChild(h("h3", { text: "Waiting for you" }));
        p.pending.forEach(function (a) {
          var answer = function (allow, always) {
            var body = { request_id: a.request_id, allow: allow, always: !!always };
            var send = function () {
              return api("/api/procs/" + id + "/permission", { method: "POST", body: body }).then(function () { toast(allow ? "Allowed" : "Denied"); refresh(); });
            };
            if (!allow) {
              ask("Deny " + a.tool, [{ name: "why", label: "Tell the agent why (optional)", optional: true }], "Deny", function (v) { body.message = v.why; return send(); });
              return;
            }
            send().catch(function (e) { toast(e.message, true); });
          };
          var input = JSON.stringify(a.input, null, 2);
          approvals.appendChild(h("div", { class: "ask" },
            h("div", { class: "row" }, h("b", { text: a.tool }), h("span", { class: "muted small", text: ago(a.ts) })),
            h("pre", { class: "view small", text: input.length > 1500 ? input.slice(0, 1500) + "\n..." : input }),
            h("div", { class: "row" },
              h("button", { type: "button", class: "primary", disabled: ro, onclick: function () { answer(true); } }, "Allow"),
              (a.suggestions || []).length ? h("button", { type: "button", disabled: ro, title: "Allow, and add Claude Code's suggested rule for the rest of this session", onclick: function () { answer(true, true); } }, "Allow always") : null,
              h("button", { type: "button", class: "danger", disabled: ro, onclick: function () { answer(false); } }, "Deny"))));
        });
      }
      controlCard.hidden = !p.interactive;
      if (!p.interactive) return;
      cv.state.textContent = "Mode " + (p.mode || "default") + ", model " + (p.model || "default") + (p.open ? "" : " (finished)");
      if (!ctlFilled && p.mode) { cv.mode.value = p.mode; ctlFilled = true; }
      var off = ro || !p.open;
      Array.prototype.forEach.call(controlCard.querySelectorAll("button,select,input,textarea"), function (x) { x.disabled = off; });
      cv.note.textContent = ro ? "Read-only console." : p.open ? "Changes apply to the next tool call or turn." : "This agent has finished its work. Use Pivot to continue it.";
    }
    m.appendChild(top);
    m.appendChild(approvals);
    m.appendChild(h("div", { class: "split" },
      h("div", null, h("div", { class: "row" }, h("h2", { text: "Live feed" }), h("label", { class: "row muted" }, follow, "Follow")), feed),
      h("div", null, controlCard, ctxCard, h("div", { class: "card" }, h("h3", { text: "Subagents" }), agentsBox, h("h3", { text: "Tool calls" }), toolsBox),
        S.ov.read_only ? null : pivotCard)));
    function updatePivot(p, children) {
      var hasSession = !!p.session && p.can_resume;
      pv.cont.disabled = !hasSession;
      // Follow the session until the user picks a mode themselves: an agent
      // reports its session a moment after it starts.
      if (!hasSession && pv.cont.checked) { pv.fresh.checked = true; }
      else if (hasSession && !pv.chosen) { pv.cont.checked = true; }
      keepRow.hidden = p.status !== "running";
      pv.note.textContent = hasSession
        ? "Runs on " + p.node + " in the same directory with " + p.runner + ". Session " + p.session.slice(0, 8) + "..."
        : !p.can_resume
          ? "Runner " + p.runner + " cannot continue a session, so a pivot starts fresh with this agent's result as context."
          : "This agent has not reported a session yet (only Claude Code does), so a pivot starts fresh with its result as context.";
      lineage.textContent = "";
      if (p.parent) lineage.appendChild(h("span", null, "Pivoted from ", h("a", { href: "#/proc/" + p.parent, text: "#" + p.parent }), p.resumed ? " (same session)" : " (fresh)"));
      if (children.length) lineage.appendChild(h("span", null, (p.parent ? "  -  " : "") + "Pivots: ", children.map(function (c, i) {
        return [i ? ", " : "", h("a", { href: "#/proc/" + c.id, text: "#" + c.id })];
      })));
    }
    var lineage = h("p", { class: "muted lineage" });
    function renderInfo(p) {
      top.textContent = "";
      var stop = h("button", { class: "danger", type: "button", disabled: p.status !== "running" || S.ov.read_only, onclick: function () {
        if (!window.confirm("Stop this agent? What it is doing now is interrupted; its session can be continued later.")) return;
        stop.disabled = true;
        api("/api/procs/" + id + "/stop", { method: "POST" }).then(function () { toast("Stopping"); }).catch(function (e) { toast(e.message, true); });
      } }, "Stop");
      var shellBtn = S.ov.shell && p.node === "local" && p.session
        ? termLink(p.status === "running" ? "Fork in terminal" : "Open in terminal", { what: "agent", proc: p.id })
        : null;
      if (shellBtn) shellBtn.title = p.status === "running" ? "Opens a branch of its session (--fork-session), so the agent is not disturbed" : "Continues its session interactively";
      top.appendChild(header("Process #" + p.id + " - " + p.runner + (p.node !== "local" ? " on " + p.node : ""), (p.host && p.node !== "local" ? p.host + ":" : "") + p.cwd, [statusPill(p.status), p.cost_usd != null ? h("span", { class: "pill", text: "$" + p.cost_usd.toFixed(4) }) : null,
        p.account ? h("a", { class: "pill", href: "#/accounts", title: "The account it runs with", text: /^login:/.test(p.account) ? "this machine's login" : "account " + p.account }) : null, shellBtn, stop]));
      top.appendChild(h("div", { class: "cmd", text: p.prompt }));
      top.appendChild(lineage);
      agentsBox.textContent = "";
      agentsBox.appendChild(p.agents.length ? h("div", { class: "bars" }, p.agents.map(function (a) {
        return h("div", null, h("div", { class: "row" }, h("b", { text: a.type }), statusPill(a.state === "running" ? "running" : a.state === "error" ? "failed" : "exited"),
          h("span", { class: "muted", text: a.tools + " tools" })), h("div", { class: "muted", text: a.description }));
      })) : h("p", { class: "muted", text: p.runner === "claude" ? "None spawned yet." : "This runner does not report subagents." }));
      toolsBox.textContent = "";
      toolsBox.appendChild(bars(p.tools));
    }
    function refresh() {
      Promise.all([api("/api/procs/" + id), api("/api/procs")]).then(function (r) {
        renderInfo(r[0]);
        renderControl(r[0]);
        renderContext(r[0]);
        updatePivot(r[0], r[1].filter(function (x) { return x.parent === Number(id); }).reverse());
      }).catch(function (e) { top.textContent = ""; top.appendChild(empty(e.message)); });
    }
    refresh();
    var es = new EventSource("/api/procs/" + id + "/events");
    var pending = null;
    es.onmessage = function (msg) {
      var e; try { e = JSON.parse(msg.data); } catch (x) { return; }
      feed.appendChild(h("div", { class: "ev " + e.kind }, h("time", { text: clock(e.ts) }), h("span", { class: "k", text: e.kind }),
        h("div", { class: "t" }, e.agent ? h("span", { class: "a", text: e.agent }) : null, e.text)));
      while (feed.childNodes.length > 3000) feed.removeChild(feed.firstChild);
      if (follow.checked) feed.scrollTop = feed.scrollHeight;
      if (["agent", "tool", "exit", "result", "permission", "system", "user"].indexOf(e.kind) >= 0) {
        clearTimeout(pending); pending = setTimeout(refresh, 300);
      }
    };
    S.cleanup.push(function () { es.close(); clearTimeout(pending); });
  }

  // Claude Code's permission modes, with what each means.
  var MODES = [["default", "default: ask before risky tools"], ["manual", "manual: ask before every tool"], ["acceptEdits", "acceptEdits: file edits run, the rest asks"],
    ["auto", "auto: Claude decides what is safe"], ["plan", "plan: read and plan, change nothing"],
    ["dontAsk", "dontAsk: deny whatever would ask"], ["bypassPermissions", "bypassPermissions: run everything, never ask (risky)"]];
  // Picking "run everything" anywhere says what that means, next to the select.
  document.addEventListener("change", function (e) {
    var sel = e.target;
    if (!sel || sel.tagName !== "SELECT" || !sel.querySelector('option[value="bypassPermissions"]')) return;
    var next = sel.nextElementSibling, warn = next && next.classList.contains("bypass-warn") ? next : null;
    if (sel.value === "bypassPermissions") {
      if (!warn) sel.insertAdjacentElement("afterend", h("div", { class: "note bad bypass-warn", role: "alert", text: "The agent will run any command and edit any file without asking, as this machine's user. Use it only in a sandbox or a throwaway checkout." }));
    } else if (warn) warn.remove();
  });
  // Model suggestions: Claude's, then whatever the local Ollama server has.
  function modelList() {
    if ($("models")) return null;
    var dl = h("datalist", { id: "models" }, ["opus", "sonnet", "haiku", "fable", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"]
      .map(function (x) { return h("option", { value: x }); }));
    ollamaModels().then(function (r) {
      (r.models || []).forEach(function (x) { dl.appendChild(h("option", { value: x.name, label: "Ollama" + (x.parameters ? ", " + x.parameters : "") })); });
      if (!(r.models || []).length) ["qwen3-coder", "gpt-oss:20b"].forEach(function (x) { dl.appendChild(h("option", { value: x })); });
    });
    return dl;
  }
  var ollamaCache = {};
  function ollamaModels(url) {
    var k = url || "";
    if (!ollamaCache[k]) ollamaCache[k] = api("/api/ollama" + (url ? hq({ url: url }) : "")).catch(function (e) { return { models: [], error: e.message }; });
    return ollamaCache[k];
  }

  // Whether this console may edit indra.toml (see Settings).
  // Editing settings, workflows and presets: admins, when the file can be edited.
  function canEdit() { return !!(S.ov && S.ov.is_admin && S.ov.settings && S.ov.settings.editable); }
  // Append one item to a list section of indra.toml.
  function addToSection(section, item) {
    return api("/api/config").then(function (c) {
      if (c.error) throw new Error("indra.toml has an error; fix it in Settings > File first");
      var list = (c.config[section] || []).filter(function (x) { return x.name !== item.name; }).concat([item]);
      return api("/api/config", { method: "PUT", body: { version: c.version, section: section, value: list } });
    }).then(function (r) { refreshOverview(); return r; });
  }

  // ---- board: work items in columns, run by agents, synced with trackers --------
  var COLS = [["todo", "To do"], ["running", "Running"], ["review", "Review"], ["done", "Done"], ["failed", "Failed"]];
  function pageBoard(m) {
    var ov = S.ov, ro = ov.read_only;
    var importBox = h("div", { class: "row" });
    var add = h("button", { class: "primary", type: "button", disabled: ro }, "New item");
    var exportBtn = h("button", { type: "button", onclick: function () {
      if (!data) return;
      jsonFile((cur === "main" ? "board" : "board-" + cur) + ".json", mine().map(function (it) { return { title: it.title, body: it.body, labels: it.labels, column: it.column, profile: it.profile, runner: it.runner, node: it.node, target: it.target, source: it.source, result: it.result }; }));
    } }, "Export");
    var importFile = h("button", { type: "button", disabled: ro, onclick: function () {
      pickFiles({ accept: ".json,application/json" }).then(function (fs) {
        if (!fs.length) return;
        var got; try { got = listFrom(fs[0].text); } catch (e) { toast(fs[0].name + ": " + e.message, true); return; }
        got = got.filter(function (x) { return x && x.title; });
        Promise.all(got.map(function (x) {
          var b = { title: String(x.title), body: x.body || "", labels: x.labels || [], board: cur };
          ["profile", "runner", "node", "target"].forEach(function (k) { if (x[k]) b[k] = x[k]; });
          if (x.column && ["todo", "review", "done"].indexOf(x.column) >= 0) b.column = x.column;
          return api("/api/board/items", { method: "POST", body: b });
        })).then(function () { toast("Imported " + got.length + " item(s)"); load(); }).catch(function (e) { toast(e.message, true); load(); });
      });
    } }, "Import file");
    m.appendChild(header("Board", "Work for agents. Run an item with a profile; when the agent finishes it moves to Review (or Failed), and you move it to Done. Items from Jira, ClickUp or GitHub report every move back.", [importBox, exportBtn, importFile, add]));
    var boardBar = h("div", { class: "row boardbar", role: "tablist", "aria-label": "Boards" });
    var lanes = h("div", { class: "lanes" }), detail = h("div");
    m.appendChild(boardBar); m.appendChild(lanes); m.appendChild(detail);
    var data = null, open = null, cur = store("indra.board") || "main";
    function mine() { return data.items.filter(function (i) { return (i.board || "main") === cur; }); }
    function boardName(id) { var b = (data.boards || []).find(function (x) { return x.id === id; }); return b ? b.name : id; }
    function pick(id) { cur = id; store("indra.board", id); showDetail(null); render(); }
    function newBoard() {
      var nm = h("input", { type: "text", required: true, "aria-label": "Board name", placeholder: "e.g. API team, Q3 pentest, Bugs" });
      var ds = h("input", { type: "text", "aria-label": "Description", placeholder: "What it is for (optional)" });
      detail.textContent = "";
      detail.appendChild(h("form", { class: "form card", onsubmit: function (e) {
        e.preventDefault();
        api("/api/boards", { method: "POST", body: { name: nm.value, description: ds.value } })
          .then(function (r) { toast("Board " + nm.value + " created"); detail.textContent = ""; cur = r.id; store("indra.board", r.id); load(); })
          .catch(function (err) { toast(err.message, true); });
      } }, h("h2", { text: "New board" }), h("label", null, "Name", nm), h("label", null, "Description", ds),
        h("div", { class: "row" }, h("button", { class: "primary", type: "submit" }, "Create"), h("button", { type: "button", onclick: function () { detail.textContent = ""; } }, "Cancel"))));
      nm.focus();
    }
    function boardSettings() {
      var b = (data.boards || []).find(function (x) { return x.id === cur; }); if (!b) return;
      var nm = h("input", { type: "text", required: true, value: b.name, "aria-label": "Board name" });
      var ds = h("input", { type: "text", value: b.description || "", "aria-label": "Description" });
      var checks = data.connectors.map(function (c) {
        var owner = (data.boards || []).find(function (x) { return (x.connectors || []).indexOf(c.name) >= 0; });
        var cb = h("input", { type: "checkbox", value: c.name });
        cb.checked = (b.connectors || []).indexOf(c.name) >= 0;
        return { cb: cb, el: h("label", { class: "check" }, cb, " " + c.name + " (" + c.kind + " " + c.project + ")",
          owner && owner.id !== b.id ? h("span", { class: "muted small", text: "  now imports into " + owner.name }) : null) };
      });
      var others = (data.boards || []).filter(function (x) { return x.id !== b.id; });
      var moveTo = h("select", { "aria-label": "Move its items to" }, others.map(function (x) { return h("option", { value: x.id, text: "Move its items to " + x.name }); }),
        h("option", { value: "", text: "Delete its items with it" }));
      var count = mine().length;
      detail.textContent = "";
      detail.appendChild(h("form", { class: "form card", onsubmit: function (e) {
        e.preventDefault();
        api("/api/boards/" + encodeURIComponent(b.id), { method: "PUT", body: { name: nm.value, description: ds.value,
          connectors: checks.filter(function (x) { return x.cb.checked; }).map(function (x) { return x.cb.value; }) } })
          .then(function () { toast("Saved board " + nm.value); detail.textContent = ""; load(); }).catch(function (err) { toast(err.message, true); });
      } }, h("h2", { text: "Board: " + b.name }), h("label", null, "Name", nm), h("label", null, "Description", ds),
        h("fieldset", null, h("legend", { text: "Import from these trackers" }),
          checks.length ? checks.map(function (x) { return x.el; }) : h("p", { class: "muted small" }, "No trackers connected yet. ", canEdit() ? h("a", { href: "#/settings/connectors", text: "Connect Jira, ClickUp, GitHub or Linear" }) : null),
          h("p", { class: "muted small", text: "Items a tracker imports land on the board that has it ticked (each tracker feeds one board); unticked trackers feed the first board." })),
        h("div", { class: "row" }, h("button", { class: "primary", type: "submit" }, "Save"), h("button", { type: "button", onclick: function () { detail.textContent = ""; } }, "Cancel")),
        b.id === "main" ? h("p", { class: "muted small", text: "The first board cannot be deleted; rename it if you like." }) :
          h("div", { class: "row danger-zone" }, count ? moveTo : null, h("button", { type: "button", class: "danger", onclick: function () {
            var to = count ? moveTo.value : "";
            if (!window.confirm("Delete board " + b.name + "?" + (count ? (to ? " Its " + count + " item(s) move to " + boardName(to) + "." : " Its " + count + " item(s) are deleted (tracker items are not touched).") : ""))) return;
            api("/api/boards/" + encodeURIComponent(b.id) + (to ? "?move_to=" + encodeURIComponent(to) : ""), { method: "DELETE" })
              .then(function () { toast("Deleted board " + b.name); detail.textContent = ""; pick(to || "main"); load(); }).catch(function (err) { toast(err.message, true); });
          } }, "Delete board"))));
      nm.focus();
    }
    function patch(id, body) {
      return api("/api/board/items/" + id, { method: "PATCH", body: body }).then(load).catch(function (e) { toast(e.message, true); });
    }
    function runForm(it) {
      var prof = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: it.profile ? "item: " + it.profile : "connector or runner default" }),
        (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); }));
      var node = h("select", { "aria-label": "Node" }, ov.nodes.map(function (n) { return h("option", { value: n, text: n }); }));
      node.value = it.node || "local";
      var target = h("input", { type: "text", placeholder: it.target || "the harness's first target", "aria-label": "Target" });
      var runner = h("select", { "aria-label": "Runner" }, h("option", { value: "", text: "the profile's runner" }),
        (ov.runners || []).map(function (r) { return h("option", { value: r.name, text: r.name }); }));
      var go = h("button", { class: "primary", type: "submit" }, "Run");
      return h("form", { class: "row runform", onsubmit: function (e) {
        e.preventDefault(); go.disabled = true;
        api("/api/board/items/" + it.id + "/run", { method: "POST", body: { profile: prof.value || null, runner: runner.value || null, node: node.value, target: target.value.trim() || null } })
          .then(function (r) { toast("Agent process #" + r.proc + " started"); load(); }).catch(function (err) { toast(err.message, true); go.disabled = false; });
      } }, prof, runner, node, target, go);
    }
    function editItem(it) {
      var f = { title: h("input", { type: "text", value: it.title, required: true }), body: h("textarea", null),
        labels: h("input", { type: "text", value: (it.labels || []).join(", "), placeholder: "comma separated" }),
        profile: h("select", null, h("option", { value: "", text: "none" }), (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); })),
        target: h("input", { type: "text", value: it.target || "", placeholder: "the harness's first target" }),
        board: h("select", null, (data.boards || []).map(function (b) { return h("option", { value: b.id, text: b.name }); })) };
      f.body.value = it.body || ""; f.profile.value = it.profile || ""; f.board.value = it.board || "main";
      detail.textContent = "";
      detail.appendChild(h("form", { class: "form card editing", onsubmit: function (e) {
        e.preventDefault();
        api("/api/board/items/" + it.id, { method: "PATCH", body: { title: f.title.value, body: f.body.value, labels: csv(f.labels.value), profile: f.profile.value, target: f.target.value.trim(), board: f.board.value } })
          .then(function (x) { toast("Saved #" + it.id); load(); showDetail(x); }).catch(function (err) { toast(err.message, true); });
      } }, h("h3", { text: "Edit #" + it.id }), h("label", null, "Title", f.title), h("label", null, "Description", f.body), h("label", null, "Labels", f.labels),
        h("div", { class: "row" }, h("label", null, "Profile", f.profile), h("label", null, "Target", f.target), (data.boards || []).length > 1 ? h("label", null, "Board", f.board) : null),
        it.source ? h("p", { class: "muted small", text: "Changes stay on the board; the item in " + it.source.connector + " is not edited." }) : null,
        h("div", { class: "row" }, h("button", { type: "submit", class: "primary" }, "Save"), h("button", { type: "button", onclick: function () { showDetail(it); } }, "Cancel"))));
    }
    // The open item's panel, redrawn by the refresh only when the item changed
    // and nobody is typing in it (an edit form or the Run row survives).
    var shownSig = null;
    function showDetail(it, fromRefresh) {
      if (fromRefresh) {
        var sig = JSON.stringify(it);
        var a = document.activeElement;
        if (sig === shownSig || detail.querySelector("form.editing") || (a && detail.contains(a) && /^(INPUT|TEXTAREA|SELECT)$/.test(a.tagName))) return;
      }
      open = it ? it.id : null; detail.textContent = ""; shownSig = it ? JSON.stringify(it) : null;
      if (!it) return;
      var src = it.source;
      detail.appendChild(h("div", { class: "card itemdetail" },
        h("div", { class: "row" }, h("h2", { text: "#" + it.id + " " + it.title }), src && safeUrl(src.url) ? h("a", { href: safeUrl(src.url), target: "_blank", rel: "noopener noreferrer", text: src.connector + " " + src.key + (src.state ? " (" + src.state + ")" : "") }) : null,
          h("button", { type: "button", class: "ghost", onclick: function () { showDetail(null); } }, "Close")),
        it.body ? h("pre", { class: "view small", text: it.body }) : h("p", { class: "muted", text: "No description." }),
        it.column !== "running" && !ro ? runForm(it) : null,
        it.proc_id ? h("p", null, h("a", { href: "#/proc/" + it.proc_id, text: "Open agent process #" + it.proc_id })) : null,
        it.result ? [h("h3", { text: "Result" }), h("pre", { class: "view small", text: it.result })] : null,
        it.notes.length ? [h("h3", { text: "History" }), h("ul", { class: "links" }, it.notes.slice().reverse().map(function (n) { return h("li", null, h("span", { class: "muted", text: ago(n.ts) + "  " }), n.text); }))] : null,
        ro || it.column === "running" ? null : h("div", { class: "row" }, h("label", { class: "inline" }, "Move to ", (function () {
            var mv = h("select", { onchange: function () { patch(it.id, { column: mv.value }); } }, COLS.filter(function (c) { return c[0] !== "running"; }).map(function (c) { return h("option", { value: c[0], text: c[1] }); }));
            mv.value = it.column; return mv;
          })()), h("button", { type: "button", onclick: function () { editItem(it); } }, "Edit"), h("button", { type: "button", class: "danger", onclick: function () {
          if (!window.confirm("Remove #" + it.id + " from the board? (Its tracker item is not touched.)")) return;
          api("/api/board/items/" + it.id, { method: "DELETE" }).then(function () { showDetail(null); load(); }).catch(function (e) { toast(e.message, true); });
        } }, "Remove from board"))));
      if (!fromRefresh) detail.scrollIntoView({ behavior: "smooth", block: "nearest" });
    }
    function card(it) {
      var c = h("div", { class: "bcard " + it.column, draggable: ro || it.column === "running" ? null : "true", tabindex: "0", role: "button",
        onclick: function () { showDetail(it); }, onkeydown: function (e) { if (e.key === "Enter") showDetail(it); } },
        h("b", { text: it.title }),
        h("div", { class: "row small" },
          it.source ? h("span", { class: "pill", text: it.source.connector + " " + it.source.key }) : h("span", { class: "pill", text: "#" + it.id }),
          it.profile ? h("span", { class: "pill", text: it.profile }) : null,
          it.column === "running" ? h("span", { class: "pill run live", text: "agent #" + it.proc_id }) : null));
      c.addEventListener("dragstart", function (e) { e.dataTransfer.setData("text/plain", String(it.id)); c.classList.add("dragging"); });
      c.addEventListener("dragend", function () { c.classList.remove("dragging"); });
      return c;
    }
    function render() {
      if (!(data.boards || []).some(function (b) { return b.id === cur; })) cur = "main";
      boardBar.textContent = "";
      (data.boards || []).forEach(function (b) {
        var n = data.items.filter(function (i) { return (i.board || "main") === b.id; }).length;
        boardBar.appendChild(h("button", { type: "button", role: "tab", class: "boardtab" + (b.id === cur ? " on" : ""), "aria-selected": b.id === cur ? "true" : "false",
          title: b.description || b.name, onclick: function () { pick(b.id); } }, b.name, h("span", { class: "muted", text: " " + n })));
      });
      if (!ro) {
        boardBar.appendChild(h("button", { type: "button", class: "ghost", onclick: newBoard }, "+ New board"));
        boardBar.appendChild(h("button", { type: "button", class: "ghost", title: "Rename, choose its trackers, or delete it", onclick: boardSettings }, "Board settings"));
      }
      lanes.textContent = "";
      COLS.forEach(function (col) {
        var items = mine().filter(function (i) { return i.column === col[0]; });
        var lane = h("div", { class: "lane " + col[0] }, h("h3", null, col[1], h("span", { class: "muted", text: " " + items.length })), items.map(card));
        if (col[0] !== "running" && !ro) {
          lane.addEventListener("dragover", function (e) { e.preventDefault(); lane.classList.add("over"); });
          lane.addEventListener("dragleave", function () { lane.classList.remove("over"); });
          lane.addEventListener("drop", function (e) {
            e.preventDefault(); lane.classList.remove("over");
            var id = Number(e.dataTransfer.getData("text/plain"));
            var it = data.items.find(function (x) { return x.id === id; });
            if (it && it.column !== col[0]) patch(id, { column: col[0] });
          });
        }
        lanes.appendChild(lane);
      });
      importBox.textContent = "";
      // A tracker imports into its own board: offer it on that board only.
      data.connectors.filter(function (c) {
        var owner = (data.boards || []).find(function (b) { return (b.connectors || []).indexOf(c.name) >= 0; });
        return (owner ? owner.id : "main") === cur;
      }).forEach(function (c) {
        importBox.appendChild(h("button", { type: "button", disabled: ro, title: c.kind + " " + c.project + (c.auto_run ? ", runs new items automatically" : ""), onclick: function (e) {
          e.target.disabled = true;
          api("/api/board/import", { method: "POST", body: { connector: c.name } })
            .then(function (r) { toast(c.name + ": " + r.added + " new, " + r.updated + " updated"); load(); })
            .catch(function (err) { toast(err.message, true); }).then(function () { e.target.disabled = false; });
        } }, "Import from " + c.name));
      });
      if (!data.connectors.length && canEdit()) importBox.appendChild(h("a", { href: "#/settings/connectors", text: "Connect Jira, ClickUp or GitHub" }));
      if (open) { var it = data.items.find(function (x) { return x.id === open; }); if (it) showDetail(it, true); }
    }
    function load() { return api("/api/board").then(function (d) { data = d; render(); }).catch(function (e) { lanes.textContent = e.message; }); }
    add.addEventListener("click", function () {
      var title = h("input", { type: "text", required: true, "aria-label": "Title", placeholder: "What should an agent do?" });
      var body = h("textarea", { "aria-label": "Details", placeholder: "Details, links, acceptance criteria" });
      var prof = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: "No profile" }), (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); }));
      detail.textContent = "";
      detail.appendChild(h("form", { class: "form card", onsubmit: function (e) {
        e.preventDefault();
        api("/api/board/items", { method: "POST", body: { title: title.value, body: body.value, profile: prof.value || null, board: cur } })
          .then(function () { detail.textContent = ""; load(); }).catch(function (err) { toast(err.message, true); });
      } }, h("h2", { text: "New item on " + boardName(cur) }), h("label", null, "Title", title), h("label", null, "Details", body), h("label", null, "Profile", prof),
        h("div", { class: "row" }, h("button", { class: "primary", type: "submit" }, "Add to To do"), h("button", { type: "button", onclick: function () { detail.textContent = ""; } }, "Cancel"))));
      title.focus();
    });
    load(); every(3000, function () { if (!document.querySelector(".lane.over")) load(); });
  }

  // ---- accounts: what each set of credentials used, and what routing gives it ------
  function pageAccounts(m) {
    m.appendChild(header("Accounts", "The credentials agents run with, what each used today and this month, and which routing can still give runs to.",
      [canEdit() ? h("a", { class: "btn", href: "#/settings/accounts", text: "Add or edit accounts" }) : null]));
    var top = h("div"), box = h("div");
    m.appendChild(top); m.appendChild(box);
    function money(x) { return "$" + (x || 0).toFixed(2); }
    function bar(f) { var b = h("div", { class: "meter" }, h("i")); b.firstChild.style.width = Math.round((f == null ? 1 : f) * 100) + "%"; if (f != null && f < 0.2) b.classList.add("low"); return b; }
    function load() {
      api("/api/accounts").then(function (d) {
        top.textContent = ""; box.textContent = "";
        var r = d.routing || {};
        var own = (d.accounts || []).filter(function (a) { return !a.login; });
        top.appendChild(h("p", { class: "muted" }, own.length
          ? "Routing: " + ({ most_remaining: "the account with the most left", round_robin: "round robin", in_order: "in order of priority" })[r.strategy] + ". When all are used up: " + (r.fallback_to_login ? "the machine's own login." : "the launch fails.") + " After a usage limit an account rests " + r.cooldown_minutes + " minutes (or until the reset the agent names)."
          : "No accounts yet: every agent uses the login its CLI has on the machine it runs on, and the usage of each such login is counted below. Add accounts to run agents with credentials from here (remote nodes then need no login), with limits and routing between them."));
        if (!(d.accounts || []).length) { box.appendChild(empty("Nothing has run yet.")); return; }
        box.appendChild(h("div", { class: "tablewrap" }, h("table", null,
          h("thead", null, h("tr", null, ["Account", "Agent", "Credential", "Today", "This month", "Left", "Status", ""].map(function (t) { return h("th", { text: t }); }))),
          h("tbody", null, d.accounts.map(function (a) {
            var lim = a.limits || {};
            return h("tr", null,
              h("td", null, h("b", { text: a.login ? "this machine's login" : a.name }), a.login ? h("div", { class: "muted small", text: a.key.replace(/^login:/, "") }) : a.description ? h("div", { class: "muted small", text: a.description }) : null),
              h("td", { text: a.agent }),
              h("td", { class: "muted small", text: a.login ? "the CLI's own login" : (a.credential ? "secret " + a.credential : "") + (a.env ? " in " + a.env : "") }),
              h("td", { text: a.today.runs + " runs, " + money(a.today.usd) + (lim.daily_usd ? " of " + money(lim.daily_usd) : "") + (lim.daily_runs ? " (of " + lim.daily_runs + " runs)" : "") }),
              h("td", { text: money(a.month_usd) + (lim.monthly_usd ? " of " + money(lim.monthly_usd) : "") }),
              h("td", null, a.login ? h("span", { class: "muted", text: "no limit" }) : bar(a.left)),
              h("td", null, h("span", { class: "pill " + (a.status === "ready" ? "ok" : a.status === "off" ? "" : "warn"), text: a.status }), a.why ? h("div", { class: "muted small", text: a.why }) : null,
                a.last_limit ? h("div", { class: "muted small clip1", title: a.last_limit, text: "last limit: " + a.last_limit }) : null),
              h("td", null, a.cooldown_until && !S.ov.read_only ? h("button", { type: "button", onclick: function () {
                api("/api/accounts/" + encodeURIComponent(a.key) + "/wake", { method: "POST" }).then(function () { toast("It can take runs again"); load(); }).catch(function (e) { toast(e.message, true); });
              } }, "Let it run again") : null));
          })))));
        box.appendChild(h("p", { class: "muted small", text: "Costs are what agents report (Claude Code reports each run's cost; subscriptions and some agents report none, so count runs for them). An account hits its limit when its agent fails saying so." }));
      }).catch(function (e) { box.textContent = ""; box.appendChild(empty(e.message)); });
    }
    load(); every(5000, load);
  }

  // ---- workflow editor: a graph of tasks, dragged and connected -------------------
  // Tasks are boxes; an arrow from A to B means B runs after A (B.needs has A).
  // The palette adds blank tasks, components (reusable tasks) and other
  // workflows (run as one step). Saving writes ordinary [[workflow]] TOML,
  // positions included, so `indra run` and the text editor keep working.
  // ---- conditions and schedules, in plain words -------------------------------------
  // A condition in indra's syntax, read back as the few simple shapes the
  // builder offers (anything else stays "written by hand").
  function parseCond(c) {
    c = (c || "").trim(); var m;
    if (!c) return { subj: "", test: "succeeded", val: "" };
    if ((m = /^([A-Za-z0-9_-]+)\.(succeeded|failed|skipped)$/.exec(c))) return { subj: m[1], test: m[2], val: "" };
    if ((m = /^not ([A-Za-z0-9_-]+)\.output contains '([^']*)'$/.exec(c))) return { subj: m[1], test: "lacks", val: m[2] };
    if ((m = /^([A-Za-z0-9_-]+)\.output contains '([^']*)'$/.exec(c))) return { subj: m[1], test: "contains", val: m[2] };
    if ((m = /^([A-Za-z0-9_-]+)\.exit_code (==|!=) (-?\d+)$/.exec(c))) return { subj: m[1], test: m[2] === "==" ? "exit" : "exitnot", val: m[3] };
    if ((m = /^input\.([A-Za-z0-9_]+) (==|!=) '([^']*)'$/.exec(c))) return { subj: "input." + m[1], test: m[2] === "==" ? "is" : "isnot", val: m[3] };
    return { custom: c };
  }
  function buildCond(x) {
    var v = (x.val || "").replace(/'/g, "");
    if (x.subj.indexOf("input.") === 0) return x.subj + (x.test === "isnot" ? " != '" : " == '") + v + "'";
    switch (x.test) {
      case "contains": return x.subj + ".output contains '" + v + "'";
      case "lacks": return "not " + x.subj + ".output contains '" + v + "'";
      case "exit": return x.subj + ".exit_code == " + (parseInt(v, 10) || 0);
      case "exitnot": return x.subj + ".exit_code != " + (parseInt(v, 10) || 0);
      default: return x.subj + "." + x.test;
    }
  }
  function condText(c) {
    var x = parseCond(c);
    if (x.custom) return x.custom;
    if (!x.subj) return "(set the condition)";
    var who = x.subj === "self" ? "its output" : x.subj.indexOf("input.") === 0 ? "input " + x.subj.slice(6) : x.subj;
    return { succeeded: who + " succeeded", failed: who + " failed", skipped: who + " was skipped",
      contains: (x.subj === "self" ? "its output" : who + "'s output") + " contains \"" + x.val + "\"", lacks: (x.subj === "self" ? "its output" : who + "'s output") + " lacks \"" + x.val + "\"",
      exit: who + " exit code is " + x.val, exitnot: who + " exit code is not " + x.val, is: who + " is \"" + x.val + "\"", isnot: who + " is not \"" + x.val + "\"" }[x.test] || c;
  }
  // Pick a subject (an earlier step, an input, or "self"), a test and a value;
  // or write the condition by hand. onChange gets the condition text.
  function condEditor(expr, subjects, inputs, selfOnly, onChange) {
    var x = parseCond(expr);
    var box = h("div", { class: "cond" });
    function draw() {
      box.textContent = "";
      if (x.custom != null) {
        var raw = h("input", { type: "text", value: x.custom, placeholder: "e.g. test.failed and input.fix == 'yes'" });
        raw.addEventListener("input", function () { x.custom = raw.value; onChange(raw.value.trim()); });
        box.appendChild(raw);
        box.appendChild(h("button", { type: "button", class: "ghost small", onclick: function () { x = parseCond(""); draw(); } }, "Use the simple form"));
        return;
      }
      var subj = h("select", { "aria-label": "What to check" }, (selfOnly ? [["self", "this step"]] : subjects.map(function (id) { return [id, "step " + id]; }).concat(inputs.map(function (n) { return ["input." + n, "input " + n]; })))
        .map(function (o) { return h("option", { value: o[0], text: o[1] }); }));
      if (!x.subj) x.subj = selfOnly ? "self" : subjects[subjects.length - 1] || (inputs[0] ? "input." + inputs[0] : "");
      subj.value = x.subj;
      var isInput = x.subj.indexOf("input.") === 0;
      var tests = isInput ? [["is", "is"], ["isnot", "is not"]] : (selfOnly ? [] : [["succeeded", "succeeded"], ["failed", "failed"], ["skipped", "was skipped"]]).concat([["contains", "output contains"], ["lacks", "output does not contain"], ["exit", "exit code is"], ["exitnot", "exit code is not"]]);
      if (!tests.some(function (t) { return t[0] === x.test; })) x.test = tests[0][0];
      var test = h("select", { "aria-label": "Test" }, tests.map(function (t) { return h("option", { value: t[0], text: t[1] }); }));
      test.value = x.test;
      var needsVal = ["contains", "lacks", "exit", "exitnot", "is", "isnot"].indexOf(x.test) >= 0;
      var val = h("input", { type: "text", value: x.val || "", placeholder: /exit/.test(x.test) ? "0" : "text", "aria-label": "Value", hidden: !needsVal });
      function emit() { onChange(x.subj ? buildCond(x) : ""); }
      subj.addEventListener("change", function () { x.subj = subj.value; draw(); emit(); });
      test.addEventListener("change", function () { x.test = test.value; draw(); emit(); });
      val.addEventListener("input", function () { x.val = val.value; emit(); });
      if (!subjects.length && !inputs.length && !selfOnly) box.appendChild(h("p", { class: "muted small", text: "Link a step into this If first: it checks the steps before it." }));
      else box.appendChild(h("div", { class: "row condrow" }, subj, test, val));
      box.appendChild(h("button", { type: "button", class: "ghost small", onclick: function () { x = { custom: x.subj ? buildCond(x) : "" }; draw(); } }, "Write it by hand"));
    }
    draw();
    if (!expr && x.subj) onChange(buildCond(x));
    return box;
  }
  // When a workflow runs by itself: a few plain choices, or cron.
  function scheduleEditor(spec, onChange) {
    var m = /^(\d+) (\d+) \* \* (\*|1-5|[0-6])$/.exec(spec || ""), hourly = /^(\d+) \* \* \* \*$/.exec(spec || "");
    var mode = !spec ? "manual" : hourly ? "hourly" : m ? (m[3] === "*" ? "daily" : m[3] === "1-5" ? "weekdays" : "weekly") : "custom";
    var time = h("input", { type: "time", value: m ? ("0" + m[2]).slice(-2) + ":" + ("0" + m[1]).slice(-2) : "09:00", "aria-label": "At" });
    var day = h("select", { "aria-label": "Day" }, ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"].map(function (d, i) { return h("option", { value: String(i), text: d }); }));
    day.value = m && /^[0-6]$/.test(m[3]) ? m[3] : "1";
    var cron = h("input", { type: "text", value: mode === "custom" ? spec : "", placeholder: "minute hour day month weekday, e.g. 0 9 * * 1-5" });
    var pick = h("select", { "aria-label": "Runs" }, [["manual", "Only when started (Run, indra run)"], ["hourly", "Every hour"], ["daily", "Every day at"], ["weekdays", "Every weekday at"], ["weekly", "Every week on"], ["custom", "On a cron schedule"]]
      .map(function (o) { return h("option", { value: o[0], text: o[1] }); }));
    pick.value = mode;
    var box = h("div", { class: "row sched" });
    function value() {
      var t = (time.value || "09:00").split(":"), hh = parseInt(t[0], 10) || 0, mm = parseInt(t[1], 10) || 0;
      switch (pick.value) {
        case "hourly": return "0 * * * *";
        case "daily": return mm + " " + hh + " * * *";
        case "weekdays": return mm + " " + hh + " * * 1-5";
        case "weekly": return mm + " " + hh + " * * " + day.value;
        case "custom": return cron.value.trim();
        default: return "";
      }
    }
    function draw() {
      box.textContent = "";
      box.appendChild(pick);
      if (pick.value === "weekly") box.appendChild(day);
      if (["daily", "weekdays", "weekly"].indexOf(pick.value) >= 0) box.appendChild(time);
      if (pick.value === "custom") box.appendChild(cron);
    }
    [pick, time, day].forEach(function (el) { el.addEventListener("change", function () { draw(); onChange(value()); }); });
    cron.addEventListener("input", function () { onChange(value()); });
    draw();
    return box;
  }
  function schedText(spec) {
    if (!spec) return "";
    var m = /^(\d+) (\d+) \* \* (\*|1-5|[0-6])$/.exec(spec), at = m ? ("0" + m[2]).slice(-2) + ":" + ("0" + m[1]).slice(-2) : "";
    if (/^\d+ \* \* \* \*$/.test(spec)) return "every hour";
    if (m) return m[3] === "*" ? "every day " + at : m[3] === "1-5" ? "weekdays " + at : ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][+m[3]] + " " + at;
    return "cron " + spec;
  }

  function pageWorkflowEditor(m, name) {
    var c = null, wf = null, original = name || null, sel = null, selEdge = null, selLoop = null, dirty = false, skills = [];
    // Where the workflow lives: { kind: "config" } (this machine's indra.toml) or
    // { kind: "repo", repo: dir, file: path } (committed with a repository).
    var loc = { kind: "config" }, was = null;
    var palette = h("div", { class: "palette", "aria-label": "Add to the workflow" });
    var scroller = h("div", { class: "canvas-scroll" });
    var canvas = h("div", { class: "canvas", role: "application", "aria-label": "Workflow graph" });
    var svg = document.createElementNS("http://www.w3.org/2000/svg", "svg"); svg.setAttribute("class", "edges");
    canvas.appendChild(svg); scroller.appendChild(canvas);
    var inspector = h("div", { class: "inspector card" });
    var status = h("span", { class: "muted small" });
    var save = h("button", { class: "primary", type: "button" }, "Save");
    var layoutBtn = h("button", { type: "button" }, "Tidy layout");
    m.appendChild(header(original ? "Edit workflow " + original : "New workflow", "Drag steps in from the left and link them: drag from a step's right edge to the next step. An If has a Yes and a No edge; drop For each or Repeat on a step to loop it; click Start to schedule the workflow. Select a step or an arrow and press Delete (or its \u00d7) to remove it.", [status, layoutBtn, save]));
    m.appendChild(h("div", { class: "wfedit" }, palette, scroller, inspector));
    m.appendChild(modelList() || h("span"));

    function changed() { dirty = true; S.dirty = true; status.textContent = "Unsaved changes"; }
    function repoFiles() { return (c.repo && c.repo.files) || []; }
    function allWorkflows() {
      var out = (c.config.workflow || []).slice();
      repoFiles().forEach(function (f) { (f.workflows || []).forEach(function (w) { if (!out.some(function (x) { return x.name === w.name; })) out.push(w); }); });
      return out;
    }
    function allComponents() {
      var out = (c.config.component || []).slice();
      repoFiles().forEach(function (f) { (f.components || []).forEach(function (x) { if (!out.some(function (y) { return y.name === x.name; })) out.push(x); }); });
      return out;
    }
    function task(id) { return wf.task.find(function (t) { return t.id === id; }); }
    function uniqueId(base) { var i = 1, id = base; while (task(id)) { i++; id = base + "-" + i; } return id; }
    function depth() {
      var memo = {};
      function d(id, seen) { if (memo[id] != null) return memo[id]; var t = task(id); if (!t || seen[id]) return 0; seen[id] = 1;
        memo[id] = (t.needs || []).length ? 1 + Math.max.apply(null, t.needs.map(function (n) { return d(n, seen); })) : 0; return memo[id]; }
      wf.task.forEach(function (t) { d(t.id, {}); });
      return memo;
    }
    function tidy(all) {
      var dp = depth(), rows = {};
      wf.task.forEach(function (t) {
        if (!all && t.pos) return;
        var col = dp[t.id] || 0; rows[col] = (rows[col] || 0) + 1;
        t.pos = [40 + col * 280, 30 + (rows[col] - 1) * 130];
      });
    }
    function reaches(from, to) { // does `to` (transitively) need `from`? then from->to... used to stop cycles
      var seen = {}, stack = [to];
      while (stack.length) { var id = stack.pop(); if (id === from) return true; if (seen[id]) continue; seen[id] = 1; var t = task(id); if (t) stack = stack.concat(t.needs || []); }
      return false;
    }
    // The Start and End blocks sit left and right of the steps (drawn only).
    var OX = 190;
    function isIf(t) { return t && t["if"] != null; }
    function kind(t) {
      if (isIf(t)) return "If " + condText(t["if"]);
      var k = t.stop ? "stop: end as " + t.stop : t.workflow ? "workflow " + t.workflow : t.run ? "$ " + t.run.split("\n")[0] : t.skill ? "skill " + t.skill : t.component ? "saved task " + t.component : "task";
      var flow = [t.on && t.on !== "success" ? "on " + t.on : "", t.when ? "only if" : ""].filter(Boolean);
      return flow.length ? k + "  [" + flow.join(", ") + "]" : k;
    }

    function draw() {
      Array.prototype.slice.call(canvas.querySelectorAll(".gnode")).forEach(function (n) { n.remove(); });
      while (svg.firstChild) svg.removeChild(svg.firstChild);
      var maxX = 900, maxY = 500;
      wf.task.forEach(function (t) {
        var n = h("div", { class: "gnode" + (sel === t.id ? " sel" : "") + (isIf(t) ? " ifstep" : "") + (t.foreach != null ? " looped" : "") + (t.stop ? " stopstep " + t.stop : t.workflow ? " sub" : t.run ? " cmdstep" : t.skill ? " skill" : t.component ? " comp" : "") + (t.on === "failure" ? " onfail" : ""), tabindex: "0", "data-id": t.id },
          h("button", { type: "button", class: "gdel", title: "Delete this step", "aria-label": "Delete step " + t.id }, "\u00d7"),
          t.foreach != null ? h("div", { class: "band each", "data-loop": "foreach", title: "Runs once per item: click to change" }, "\u21bb for each: " + (String(t.foreach).trim() || "(add items)")) : null,
          t.until != null ? h("div", { class: "band until", "data-loop": "until", title: "Runs again until this holds: click to change" }, "\u21ba repeat until " + condText(t.until) + ", at most " + (t.max_runs || 3)) : null,
          h("span", { class: "port in", title: "runs after" }),
          h("b", { text: t.id }), h("small", { text: kind(t) }),
          isIf(t) ? null : h("small", { class: "muted", text: t.stop ? (t.when ? "if " + t.when : "always, once reached") : [t.node || "local", t.profile || t.runner || ""].filter(Boolean).join(" - ") }),
          isIf(t) ? [h("span", { class: "port out yes", "data-branch": "yes", title: "Yes: drag to the step that runs when the answer is yes" }), h("span", { class: "plabel yes", text: "Yes" }),
            h("span", { class: "port out no", "data-branch": "no", title: "No: drag to the step that runs when the answer is no" }), h("span", { class: "plabel no", text: "No" })]
            : h("span", { class: "port out", title: "drag to the task that runs after this one" }));
        n.style.left = (t.pos[0] + OX) + "px"; n.style.top = t.pos[1] + "px";
        canvas.appendChild(n);
        maxX = Math.max(maxX, t.pos[0] + OX + 400); maxY = Math.max(maxY, t.pos[1] + 260);
        n.addEventListener("pointerdown", function (e) {
          if (e.target.classList.contains("gdel")) { e.stopPropagation(); e.preventDefault(); removeTask(t.id); return; }
          if (e.target.classList.contains("out")) return connect(e, t, e.target.getAttribute("data-branch"));
          var band = e.target.closest(".band");
          if (band) { e.preventDefault(); e.stopPropagation(); selectLoop(t.id, band.getAttribute("data-loop")); return; }
          select(t.id); e.preventDefault(); n.focus({ preventScroll: true });
          var sx = e.clientX, sy = e.clientY, ox = t.pos[0], oy = t.pos[1];
          try { n.setPointerCapture(e.pointerId); } catch (x) { /* a synthetic or ended pointer */ }
          function mv(ev) { t.pos = [Math.max(0, Math.round(ox + ev.clientX - sx)), Math.max(0, Math.round(oy + ev.clientY - sy))]; n.style.left = (t.pos[0] + OX) + "px"; n.style.top = t.pos[1] + "px"; drawEnds(); edges(); }
          function up() { n.removeEventListener("pointermove", mv); n.removeEventListener("pointerup", up); if (t.pos[0] !== ox || t.pos[1] !== oy) changed(); }
          n.addEventListener("pointermove", mv); n.addEventListener("pointerup", up);
        });

      });
      canvas.style.width = maxX + "px"; canvas.style.height = maxY + "px";
      svg.setAttribute("width", maxX); svg.setAttribute("height", maxY);
      drawEnds();
      edges();
    }
    // Start: before the steps that need nothing. End: after the steps nothing needs.
    function roots() { return wf.task.filter(function (t) { return !(t.needs || []).length; }); }
    function leaves() { return wf.task.filter(function (t) { return !wf.task.some(function (o) { return (o.needs || []).indexOf(t.id) >= 0; }); }); }
    function drawEnds() {
      Array.prototype.slice.call(canvas.querySelectorAll(".gend")).forEach(function (n) { n.remove(); });
      if (!wf.task.length) return;
      function midY(list) { return list.length ? list.reduce(function (a, t) { return a + t.pos[1]; }, 0) / list.length + 22 : 40; }
      var right = Math.max.apply(null, wf.task.map(function (t) { return t.pos[0]; })) + OX + 250;
      [["__start", "Start", 16, midY(roots()) - (wf.schedule ? 10 : 0)], ["__end", "End", right, midY(leaves())]].forEach(function (x) {
        var n = h("div", { class: "gnode gend " + (x[0] === "__start" ? "gstart" : "gfinish"), "data-id": x[0], title: x[0] === "__start" ? "Steps that need nothing start here" : "The workflow ends when every step has finished (or a stop step ends it)" },
          h("b", { text: x[1] }), x[0] === "__start" ? h("small", { class: "muted", text: wf.schedule ? "\u23f0 " + schedText(wf.schedule) : "click to schedule" }) : null);
        n.style.left = x[2] + "px"; n.style.top = x[3] + "px";
        if (x[0] === "__start") {
          n.classList.add("click"); n.title = "When the workflow runs: click to schedule it";
          n.addEventListener("pointerdown", function (e) { e.stopPropagation(); sel = null; selEdge = null; selLoop = null; draw(); inspect(); });
        }
        canvas.appendChild(n);
      });
      var w = Math.max(parseInt(canvas.style.width, 10) || 0, right + 120);
      canvas.style.width = w + "px"; svg.setAttribute("width", w);
    }
    function center(id, side, branch) {
      var n = canvas.querySelector('.gnode[data-id="' + CSS.escape(id) + '"]'); if (!n) return null;
      var port = branch && n.querySelector(".port.out." + branch);
      if (port) return { x: n.offsetLeft + n.offsetWidth, y: n.offsetTop + port.offsetTop + port.offsetHeight / 2 };
      return { x: n.offsetLeft + (side === "out" ? n.offsetWidth : 0), y: n.offsetTop + n.offsetHeight / 2 };
    }
    function path(a, b) { var dx = Math.max(40, Math.abs(b.x - a.x) / 2); return "M" + a.x + "," + a.y + " C" + (a.x + dx) + "," + a.y + " " + (b.x - dx) + "," + b.y + " " + b.x + "," + b.y; }
    function edges() {
      while (svg.firstChild) svg.removeChild(svg.firstChild);
      function line(a, b, cls) {
        if (!a || !b) return;
        var p = document.createElementNS("http://www.w3.org/2000/svg", "path");
        p.setAttribute("d", path(a, b)); p.setAttribute("class", "edge " + cls); svg.appendChild(p);
      }
      roots().forEach(function (t) { line(center("__start", "out"), center(t.id, "in"), "implicit"); });
      leaves().forEach(function (t) { line(center(t.id, "out"), center("__end", "in"), "implicit" + (t.stop ? " " + t.stop : "")); });
      wf.task.forEach(function (t) {
        // Repeat until: an arrow from its end back to its start.
        if (t.until != null) {
          var nd = canvas.querySelector('.gnode[data-id="' + CSS.escape(t.id) + '"]');
          if (nd) {
            var lx = nd.offsetLeft, rx = lx + nd.offsetWidth, ty = nd.offsetTop + 8;
            var lp = document.createElementNS("http://www.w3.org/2000/svg", "path");
            lp.setAttribute("d", "M" + rx + "," + ty + " C" + (rx + 30) + "," + (ty - 46) + " " + (lx - 30) + "," + (ty - 46) + " " + lx + "," + ty);
            lp.setAttribute("class", "edge loopback"); svg.appendChild(lp);
          }
        }
        (t.needs || []).forEach(function (from) {
          var src = task(from), br = isIf(src) ? (t.branch || "yes") : null;
          var a = center(from, "out", br), b = center(t.id, "in"); if (!a || !b) return;
          var on = selEdge && selEdge[0] === from && selEdge[1] === t.id;
          var hit = document.createElementNS("http://www.w3.org/2000/svg", "path");
          hit.setAttribute("d", path(a, b)); hit.setAttribute("class", "hit");
          hit.addEventListener("pointerdown", function (e) { e.stopPropagation(); selEdge = [from, t.id]; sel = null; inspect(); edges(); });
          var p = document.createElementNS("http://www.w3.org/2000/svg", "path");
          p.setAttribute("d", path(a, b)); p.setAttribute("class", "edge" + (on ? " on" : "") + (br ? " " + br : "") + (t.on === "failure" ? " fail" : t.on === "always" ? " always" : ""));
          svg.appendChild(p); svg.appendChild(hit);
          if (br || (t.on && t.on !== "success")) {
            var lb = document.createElementNS("http://www.w3.org/2000/svg", "text");
            lb.setAttribute("x", (a.x + b.x) / 2); lb.setAttribute("y", (a.y + b.y) / 2 - 6); lb.setAttribute("class", "elabel " + (br || t.on));
            lb.textContent = br ? (br === "yes" ? "Yes" : "No") : t.on === "failure" ? "if it fails" : "either way";
            svg.appendChild(lb);
          }
        });
      });
    }
    function connect(e, from, branch) {
      e.preventDefault(); e.stopPropagation();
      var line = document.createElementNS("http://www.w3.org/2000/svg", "path"); line.setAttribute("class", "edge draft"); svg.appendChild(line);
      var a = center(from.id, "out", branch), box = canvas.getBoundingClientRect();
      function mv(ev) { line.setAttribute("d", path(a, { x: ev.clientX - box.left, y: ev.clientY - box.top })); }
      function up(ev) {
        document.removeEventListener("pointermove", mv); document.removeEventListener("pointerup", up); line.remove();
        var el = document.elementFromPoint(ev.clientX, ev.clientY), n = el && el.closest(".gnode");
        if (!n) return;
        var to = task(n.getAttribute("data-id"));
        if (!to || to.id === from.id) return;
        if ((to.needs || []).indexOf(from.id) >= 0) return;
        if (reaches(to.id, from.id)) { toast("That would make a loop: " + from.id + " already runs after " + to.id, true); return; }
        to.needs = (to.needs || []).concat([from.id]);
        if (branch) to.branch = branch;
        changed(); draw(); select(to.id);
      }
      document.addEventListener("pointermove", mv); document.addEventListener("pointerup", up);
    }
    function removeTask(id) {
      var gone = task(id); if (!gone) return;
      var before = JSON.parse(JSON.stringify(wf.task));
      wf.task = wf.task.filter(function (t) { return t.id !== id; });
      wf.task.forEach(function (t) { if ((t.needs || []).indexOf(id) >= 0) unlink(id, t.id); });
      sel = null; changed(); draw(); inspect();
      toastAction("Deleted step " + id, "Undo", function () { wf.task = before; sel = id; changed(); draw(); inspect(); });
    }
    // Remove a link; a step no longer after an If loses its Yes/No side.
    function unlink(from, to) {
      var t = task(to); if (!t) return;
      t.needs = (t.needs || []).filter(function (n) { return n !== from; });
      if (t.branch && !t.needs.some(function (n) { return isIf(task(n)); })) delete t.branch;
    }
    function selectLoop(id, which) { sel = id; selEdge = null; selLoop = which; Array.prototype.forEach.call(canvas.querySelectorAll(".gnode"), function (n) { n.classList.toggle("sel", n.getAttribute("data-id") === id); }); edges(); inspect(); }
    function select(id) { sel = id; selEdge = null; selLoop = null; Array.prototype.forEach.call(canvas.querySelectorAll(".gnode"), function (n) { n.classList.toggle("sel", n.getAttribute("data-id") === id); }); edges(); inspect(); }
    document.addEventListener("keydown", function onKey(e) {
      if (!document.body.contains(canvas)) { document.removeEventListener("keydown", onKey); return; }
      var typing = /INPUT|TEXTAREA|SELECT/.test(document.activeElement.tagName);
      if (sel && !selEdge && (e.key === "Delete" || e.key === "Backspace") && !typing) { removeTask(sel); e.preventDefault(); return; }
      if (selEdge && (e.key === "Delete" || e.key === "Backspace") && !typing) {
        unlink(selEdge[0], selEdge[1]);
        selEdge = null; changed(); draw(); inspect(); e.preventDefault();
      }
    });
    scroller.addEventListener("pointerdown", function (e) { if (e.target === canvas || e.target === svg || e.target === scroller) { sel = null; selEdge = null; selLoop = null; draw(); inspect(); } });

    function field(label, el, help) { return h("label", null, label, help ? h("small", { text: help }) : null, el); }
    function inspect() {
      inspector.textContent = "";
      if (selEdge) {
        var lt = task(selEdge[1]), lf = task(selEdge[0]);
        var lbox = h("div", { class: "form" }, h("h3", { text: "Link" }));
        if (isIf(lf)) {
          var side = h("select", null, h("option", { value: "yes", text: "when the answer is Yes" }), h("option", { value: "no", text: "when the answer is No" }));
          side.value = lt.branch || "yes";
          side.addEventListener("change", function () { lt.branch = side.value; changed(); draw(); });
          lbox.appendChild(field(selEdge[1] + " runs", side, "The If asks: " + condText(lf["if"]) + "?"));
        } else {
          var when = h("select", null, h("option", { value: "", text: "when it succeeds" }), h("option", { value: "failure", text: "when it fails (to report or fix)" }), h("option", { value: "always", text: "either way, once it is done (to clean up)" }));
          when.value = lt.on || "";
          when.addEventListener("change", function () { if (when.value) lt.on = when.value; else delete lt.on; changed(); draw(); });
          lbox.appendChild(field(selEdge[1] + " runs after " + selEdge[0], when, (lt.needs || []).length > 1 ? "For every step " + selEdge[1] + " runs after." : null));
        }
        lbox.appendChild(h("button", { type: "button", class: "danger", onclick: function () { unlink(selEdge[0], selEdge[1]); selEdge = null; changed(); draw(); inspect(); } }, "Remove link"));
        inspector.appendChild(lbox);
        return;
      }
      // The loop on a step: what it repeats over, or until when.
      if (sel && selLoop && task(sel)) {
        var lt2 = task(sel), lb2 = h("div", { class: "form" });
        if (selLoop === "foreach") {
          var items = h("textarea", { placeholder: "api, web, worker\n(one per line, or commas; or {input.NAME})" }); items.value = lt2.foreach == null ? "" : Array.isArray(lt2.foreach) ? lt2.foreach.join("\n") : String(lt2.foreach);
          items.addEventListener("input", function () { lt2.foreach = items.value; changed(); });
          items.addEventListener("change", draw);
          lb2.appendChild(h("h3", { text: "For each" }));
          lb2.appendChild(field("Items", items, sel + " runs once per item, side by side. Use {item} in its prompt or folder ($INDRA_ITEM in a command)."));
          lb2.appendChild(h("button", { type: "button", class: "danger", onclick: function () { delete lt2.foreach; selLoop = null; changed(); draw(); inspect(); } }, "Remove the loop"));
        } else {
          lb2.appendChild(h("h3", { text: "Repeat until" }));
          lb2.appendChild(field(sel + " runs again until", condEditor(lt2.until || "", [], [], true, function (v) { lt2.until = v; changed(); draw(); }), "Checked after each run that succeeds."));
          var mx = h("input", { type: "number", min: "1", max: "20", value: String(lt2.max_runs || 3) });
          mx.addEventListener("input", function () { lt2.max_runs = Math.max(1, Math.min(20, Number(mx.value) || 3)); changed(); draw(); });
          lb2.appendChild(field("At most (runs)", mx));
          lb2.appendChild(h("button", { type: "button", class: "danger", onclick: function () { delete lt2.until; delete lt2.max_runs; selLoop = null; changed(); draw(); inspect(); } }, "Remove the loop"));
        }
        inspector.appendChild(lb2);
        return;
      }
      var t = sel && task(sel);
      if (!t) {
        var nm = h("input", { type: "text", value: wf.name, required: true }), ds = h("input", { type: "text", value: wf.description || "" });
        nm.addEventListener("input", function () { wf.name = nm.value.trim(); changed(); });
        ds.addEventListener("input", function () { wf.description = ds.value; changed(); });
        var where = h("select", { "aria-label": "Saved in" },
          h("option", { value: "config", text: "My indra.toml (only on this machine)" }),
          ((c.repo && c.repo.repos) || []).map(function (r) { return h("option", { value: r.dir, text: r.dir + " (commit to share)" }); }));
        where.value = loc.kind === "repo" ? loc.repo : "config";
        where.addEventListener("change", function () { loc = where.value === "config" ? { kind: "config" } : { kind: "repo", repo: where.value, file: loc.kind === "repo" && loc.repo === where.value ? loc.file : null }; changed(); inspect(); });
        var file = loc.kind === "repo" ? (loc.file || (((c.repo.repos || []).find(function (r) { return r.dir === loc.repo; }) || {}).folder + "/" + (wf.name || "workflow") + ".toml")) : c.path;
        var sch = scheduleEditor(wf.schedule || "", function (v) { if (v) wf.schedule = v; else delete wf.schedule; changed(); drawEnds(); edges(); });
        inspector.appendChild(h("div", { class: "form" }, h("h3", { text: "Workflow" }), field("Name", nm, "Run it with `indra run <name>` or from the Workflows page."), field("Description", ds),
          field("Runs", sch, "On a schedule it runs by itself while the console (indra serve) runs, in this computer's time; its inputs take their defaults."),
          field("Saved in", where, "In a repository it is a file in .indra/workflows/: commit it, and everyone with that repository gets the workflow (workflows as code). Steps with no target run in that repository."),
          h("p", { class: "muted small" }, "File: ", h("code", { text: file || "" })),
          h("p", { class: "muted small", text: wf.task.length + " steps. Select a step to edit it." })));
        return;
      }
      var f = h("div", { class: "form" });
      var idIn = h("input", { type: "text", value: t.id });
      idIn.addEventListener("change", function () {
        var v = idIn.value.trim();
        if (!/^[A-Za-z0-9_-]{1,64}$/.test(v) || (v !== t.id && task(v))) { toast("Ids are unique: letters, digits, - and _", true); idIn.value = t.id; return; }
        wf.task.forEach(function (x) { x.needs = (x.needs || []).map(function (n) { return n === t.id ? v : n; }); });
        t.id = v; sel = v; changed(); draw();
      });
      f.appendChild(h("div", { class: "row inspect-head" }, h("h3", { text: isIf(t) ? "If" : t.stop ? "Stop step" : t.workflow ? "Sub-workflow step" : t.run != null ? "Command step" : t.skill ? "Skill step" : "Task" }),
        h("button", { type: "button", class: "danger small", title: "Delete this step (or press Delete)", onclick: function () { removeTask(t.id); } }, "Delete step")));
      f.appendChild(field("Id", idIn));
      function sel_(k, options, blank, help) {
        var s = h("select", null, (blank != null ? [h("option", { value: "", text: blank })] : []).concat(options.map(function (o) { return h("option", { value: o, text: o }); })));
        s.value = t[k] || ""; s.addEventListener("change", function () { if (s.value) t[k] = s.value; else delete t[k]; changed(); draw(); select(t.id); });
        return s;
      }
      function text(k, area, ph) {
        var el = area ? h("textarea", { placeholder: ph || "" }) : h("input", { type: "text", placeholder: ph || "" });
        el.value = t[k] == null ? "" : String(t[k]);
        el.addEventListener("input", function () { if (el.value.trim()) t[k] = k === "retries" ? Number(el.value) || 0 : el.value; else delete t[k]; changed(); });
        el.addEventListener("change", draw);
        return el;
      }
      // The steps before this one (what a condition may read), and the inputs.
      function earlier(id) {
        var seen = {}, stack = (task(id).needs || []).slice(), out = [];
        while (stack.length) { var x = stack.pop(); if (seen[x]) continue; seen[x] = 1; out.push(x); var tt = task(x); if (tt) stack = stack.concat(tt.needs || []); }
        return out.filter(function (x) { var tt = task(x); return tt && !isIf(tt) && tt.foreach == null; });
      }
      var inputNames = (wf.input || []).map(function (i) { return i.name; });
      if (isIf(t)) {
        f.appendChild(field("Asks", condEditor(t["if"], earlier(t.id), inputNames, false, function (v) { t["if"] = v; changed(); draw(); select(t.id); }),
          "Drag from Yes (green) to what runs when the answer is yes, and from No (red) to what runs when it is no. A step both sides lead to runs after whichever side ran."));
      } else if (t.stop) {
        f.appendChild(field("Ends the workflow as", sel_("stop", ["success", "failure"], null),
          "When this step is reached (after the steps before it, and only if its condition holds), the workflow ends at once: steps not yet started are skipped and running ones are stopped."));
        f.appendChild(field("Only if", text("when", false, "e.g. scan.output contains 'no findings'"), "Without a condition it ends the workflow as soon as it is reached."));
        f.appendChild(field("Runs", sel_("on", ["failure", "always"], "when the steps before it succeed"), "On failure: stop when a step before it failed (to end as failure, say)."));
        f.appendChild(field("Message", text("prompt", true, "Why it stopped (shown on the run page)")));
      } else if (t.workflow) {
        f.appendChild(field("Runs workflow", sel_("workflow", allWorkflows().map(function (w) { return w.name; }).filter(function (n) { return n !== wf.name; }), null),
          "Its tasks run as this one step: they start after this step's links, and what runs after this step waits for them all."));
      } else {
        var comp = allComponents(), cc = t.component && comp.find(function (x) { return x.name === t.component; });
        if (t.run != null) {
          var cmd = h("textarea", { class: "code", placeholder: "cargo test 2>&1", spellcheck: "false" }); cmd.value = t.run;
          cmd.addEventListener("input", function () { t.run = cmd.value; changed(); });
          cmd.addEventListener("change", draw);
          f.appendChild(field("Command", cmd, "Runs with bash (sh; PowerShell on Windows) in the target. Inputs, the loop item and earlier results are environment variables: $INDRA_INPUT_NAME, $INDRA_ITEM, $INDRA_OUTPUT_STEP, $INDRA_EXIT_STEP, $INDRA_STATUS_STEP."));
        } else if (t.skill) {
          var names = skills.map(function (x) { return x.name; }); if (names.indexOf(t.skill) < 0) names.unshift(t.skill);
          f.appendChild(field("Skill", sel_("skill", names, null), "Runs the skill as Claude Code's /" + t.skill + " command; the prompt below is what follows it."));
        } else {
          f.appendChild(field("Saved task", sel_("component", comp.map(function (x) { return x.name; }), "None"), "Start from a saved task: fields left empty here come from it."));
        }
        f.appendChild(field("Node", sel_("node", cfgNodeNames(c), cc && cc.node ? "component: " + cc.node : "local")));
        if (t.run == null) {
          f.appendChild(field("Profile", sel_("profile", cfgProfileNames(c), cc && cc.profile ? "component: " + cc.profile : "None")));
          f.appendChild(field("Runner", sel_("runner", cfgRunnerNames(c), cc && cc.runner ? "component: " + cc.runner : "claude (or the profile's)")));
          var mdl = text("model", false, "the profile's"); mdl.setAttribute("list", "models");
          f.appendChild(field("Model", mdl, "Over the profile's; with Ollama, a local model's name."));
          f.appendChild(field("Runs on", sel_("provider", ["anthropic", "ollama"], "the profile's choice")));
        }
        f.appendChild(field("Target", text("target", false, cc && cc.target ? cc.target : loc.kind === "repo" ? "this repository" : t.node && t.node !== "local" ? "a directory on that node (required)" : "the harness's first target"),
          loc.kind === "repo" ? "Relative paths are inside the repository." : null));
        if (t.run == null) f.appendChild(field(t.skill ? "Arguments" : "Prompt", text("prompt", true, t.skill ? "{target}" : cc && cc.prompt ? cc.prompt : "What should the agent do? {target}, {node}, {input.NAME}, {item} and {steps.STEP.output} are filled in."),
          t.skill ? "Default {target}: the directory it runs in." : "{steps.STEP.output} is an earlier step's result (it must run before this one)."));
        f.appendChild(field("Retries", text("retries", false, cc && cc.retries ? String(cc.retries) : "0")));
        var af = h("input", { type: "checkbox" }); af.checked = !!t.allow_failure;
        af.addEventListener("change", function () { if (af.checked) t.allow_failure = true; else delete t.allow_failure; changed(); draw(); select(t.id); });
        f.appendChild(h("label", { class: "check" }, af, " Keep going if this step fails"));
        f.appendChild(h("p", { class: "muted small", text: "To loop it, drop For each or Repeat until on it; to choose what follows, add an If; to run it when a step fails, click the arrow into it." }));
        if (t.when) f.appendChild(h("div", { class: "note" }, "Only if: " + condText(t.when) + " ",
          h("button", { type: "button", class: "ghost small", title: "Conditions are If blocks now: add one before this step", onclick: function () { delete t.when; changed(); draw(); select(t.id); } }, "Remove")));
      }
      f.appendChild(h("p", { class: "muted small", text: (t.needs || []).length ? "Runs after: " + t.needs.join(", ") : "Runs first (nothing before it)." }));
      f.appendChild(h("div", { class: "row" },
        !t.workflow && !t.stop && canEdit() ? h("button", { type: "button", title: "Keep this task's settings to drag into other workflows", onclick: function () {
          ask("Save this step for reuse", [{ name: "n", label: "Name", value: t.id, hint: "It appears under Saved tasks in every workflow's palette." }], "Save", function (v) { return saveComp(v.n); });
        } }, "Save for reuse") : null));
      inspector.appendChild(f);
      function saveComp(n) {
          var comp = { name: n.trim() }; ["prompt", "profile", "runner", "node", "target", "skill"].forEach(function (k) { if (t[k]) comp[k] = t[k]; }); if (t.retries) comp.retries = t.retries;
          if (loc.kind === "repo") {
            // Kept in the workflow's own file when it is saved.
            pendingComps = pendingComps.filter(function (x) { return x.name !== comp.name; }).concat([comp]);
            c.repo.files = repoFiles().concat([{ file: "", components: [comp], workflows: [] }]);
            t.component = comp.name; changed(); renderPalette(); toast("Saved task " + comp.name + " is kept with this workflow when you save it"); return;
          }
          return addToSection("component", comp).then(function (r) { c = r; toast("Saved task " + comp.name); renderPalette(); });
      }
    }

    var pendingComps = [];
    // A loop goes onto a step (the one dropped on, or the selected one).
    function addLoop(which, onto) {
      var t = onto && task(onto);
      if (!t || isIf(t) || t.stop || t.workflow) { toast("Drop " + (which === "foreach" ? "For each" : "Repeat until") + " on a task or command step", true); return; }
      if (which === "foreach") { if (t.foreach == null) t.foreach = ""; }
      else if (t.until == null) { t.until = ""; t.max_runs = t.max_runs || 3; }
      changed(); draw(); selectLoop(t.id, which);
    }
    function add(spec, x, y) {
      if (spec.loop) { addLoop(spec.loop, sel); return; }
      // Not on top of another step: move down (then right) to a free spot.
      function taken(px, py) { return wf.task.some(function (o) { return o.pos && Math.abs(o.pos[0] - px) < 200 && Math.abs(o.pos[1] - py) < 110; }); }
      for (var tries = 0; taken(x, y) && tries < 60; tries++) { y += 130; if (tries % 5 === 4) { y -= 650; x += 280; } }
      var t = { id: uniqueId((spec.workflow || spec.component || spec.skill || "task").replace(/[^A-Za-z0-9_-]/g, "-")), pos: [Math.round(x), Math.round(y)] };
      if (spec.workflow) t.workflow = spec.workflow;
      if (spec.component) t.component = spec.component;
      if (spec.skill) t.skill = spec.skill;
      if (spec.run != null) { t.run = spec.run; t.id = uniqueId("command"); }
      if (spec.stop) { t.stop = spec.stop; t.id = uniqueId("stop"); }
      if (spec.check) { t["if"] = ""; t.id = uniqueId("if"); }
      wf.task.push(t); changed(); draw(); select(t.id);
    }
    function renderPalette() {
      palette.textContent = "";
      function item(label, sub, spec) {
        var el = h("div", { class: "pitem", draggable: "true", tabindex: "0", role: "button", title: "Drag onto the canvas, or press Enter to add" }, h("b", { text: label }), sub ? h("small", { class: "muted", text: sub }) : null);
        el.addEventListener("dragstart", function (e) { e.dataTransfer.setData("application/x-indra", JSON.stringify(spec)); });
        el.addEventListener("keydown", function (e) { if (e.key === "Enter") add(spec, 40 + scroller.scrollLeft, 30 + scroller.scrollTop); });
        el.addEventListener("dblclick", function () { add(spec, 40 + scroller.scrollLeft, 30 + scroller.scrollTop); });
        return el;
      }
      palette.appendChild(h("h3", { text: "Add" }));
      palette.appendChild(item("Task", "an agent with a prompt", {}));
      palette.appendChild(item("Command", "a shell command (bash)", { run: "" }));
      palette.appendChild(item("If", "check something, then Yes or No", { check: true }));
      palette.appendChild(item("Stop", "end the workflow here", { stop: "success" }));
      palette.appendChild(h("h3", { text: "Loops", title: "Drop one on a step" }));
      palette.appendChild(item("For each", "drop on a step: once per item", { loop: "foreach" }));
      palette.appendChild(item("Repeat until", "drop on a step: again until...", { loop: "until" }));
      palette.appendChild(h("h3", { text: "Skills", title: "Run one of the harness's skills as a step" }));
      if (!skills.length) palette.appendChild(h("p", { class: "muted small", text: "This harness has no skills." }));
      var sf = null;
      if (skills.length > 6) {
        sf = h("input", { type: "search", placeholder: "Filter skills", "aria-label": "Filter skills" });
        palette.appendChild(sf);
      }
      var skillBox = h("div", { class: "pgroup" }); palette.appendChild(skillBox);
      function drawSkills() {
        skillBox.textContent = "";
        var q = sf ? sf.value.trim().toLowerCase() : "";
        skills.filter(function (x) { return !q || (x.name + " " + x.description).toLowerCase().indexOf(q) >= 0; }).slice(0, 40)
          .forEach(function (x) { skillBox.appendChild(item(x.name, x.description, { skill: x.name })); });
      }
      if (sf) sf.addEventListener("input", drawSkills);
      drawSkills();
      var comps = allComponents();
      palette.appendChild(h("h3", { text: "Saved tasks", title: "A task saved for reuse: drag it into any workflow; change it once and every workflow using it changes" }));
      if (!comps.length) palette.appendChild(h("p", { class: "muted small", text: "A task saved for reuse. Select a task and press Save for reuse." }));
      comps.forEach(function (x) { palette.appendChild(item(x.name, x.description || (x.skill ? "skill " + x.skill : x.prompt), { component: x.name })); });
      var others = allWorkflows().filter(function (w) { return w.name !== original; });
      palette.appendChild(h("h3", { text: "Workflows" }));
      if (!others.length) palette.appendChild(h("p", { class: "muted small", text: "Other workflows appear here, to run as one step." }));
      others.forEach(function (w) { palette.appendChild(item(w.name, (w.task || []).length + " steps", { workflow: w.name })); });
    }
    scroller.addEventListener("dragover", function (e) { e.preventDefault(); });
    scroller.addEventListener("drop", function (e) {
      var d = e.dataTransfer.getData("application/x-indra"); if (!d) return;
      e.preventDefault(); var box = canvas.getBoundingClientRect();
      var spec = JSON.parse(d);
      if (spec.loop) {
        var hit = document.elementFromPoint(e.clientX, e.clientY), node = hit && hit.closest(".gnode");
        addLoop(spec.loop, node && node.getAttribute("data-id"));
        return;
      }
      add(spec, e.clientX - box.left - 90 - OX, e.clientY - box.top - 30);
    });
    layoutBtn.addEventListener("click", function () { tidy(true); changed(); draw(); });
    save.addEventListener("click", function () {
      if (!wf.name || !/^[A-Za-z0-9_-]{1,64}$/.test(wf.name)) { toast("Give the workflow a name: letters, digits, - and _", true); return; }
      if (!wf.task.length) { toast("Add at least one step", true); return; }
      var clean = JSON.parse(JSON.stringify(wf));
      if (!clean.description) delete clean.description;
      var problem = null;
      clean.task.forEach(function (t) {
        if (!t.needs || !t.needs.length) delete t.needs; if (!t.retries) delete t.retries;
        if (t.foreach != null && !String(t.foreach).trim()) problem = problem || "For each on " + t.id + " has no items";
        if (t.until != null && !String(t.until).trim()) problem = problem || "Repeat on " + t.id + " has no condition";
        if (t["if"] != null && !String(t["if"]).trim()) problem = problem || "The If " + t.id + " has no condition";
        if (t.branch && !(t.needs || []).some(function (n) { return clean.task.some(function (x) { return x.id === n && x["if"] != null; }); })) delete t.branch;
      });
      if (problem) { toast(problem + ": select it to finish it, or remove it.", true); return; }
      save.disabled = true;
      // Out of the config file, when it is in there under its old name.
      function dropFromConfig(cc) {
        if (!(cc.config.workflow || []).some(function (w) { return w.name === original; })) return Promise.resolve(cc);
        return api("/api/config", { method: "PUT", body: { version: cc.version, section: "workflow", value: (cc.config.workflow || []).filter(function (w) { return w.name !== original; }) } });
      }
      var done;
      if (loc.kind === "repo") {
        // Saved tasks it uses that live only in this machine's config go into the file too.
        var mine = (c.config.component || []).filter(function (x) { return clean.task.some(function (t) { return t.component === x.name; }) &&
          !repoFiles().some(function (f) { return f.file && (f.components || []).some(function (y) { return y.name === x.name; }); }); });
        done = api("/api/workflow-files", { method: "PUT", body: { repo: loc.repo, workflow: clean, original_file: was && was.kind === "repo" ? was.file : null,
          original_name: original, components: mine.concat(pendingComps) } }).then(function (r) {
          loc.file = r.file; if (r.problem) toast("Saved, but not usable here yet: " + r.problem, true);
          return was && was.kind === "config" ? dropFromConfig(r.config) : r.config;
        });
      } else {
        var list = (c.config.workflow || []).filter(function (w) { return w.name !== original && w.name !== clean.name; });
        var at = (c.config.workflow || []).findIndex(function (w) { return w.name === original; });
        if (at >= 0 && at <= list.length) list.splice(at, 0, clean); else list.push(clean);
        done = api("/api/config", { method: "PUT", body: { version: c.version, section: "workflow", value: list } }).then(function (r) {
          // Moved here from a repository: take it out of that file.
          if (was && was.kind === "repo" && was.file) return api("/api/workflow-files/delete", { method: "POST", body: { file: was.file, name: original } });
          return r;
        });
      }
      done.then(function (r) {
        c = r; original = clean.name; was = JSON.parse(JSON.stringify(loc)); pendingComps = []; dirty = false; status.textContent = "Saved";
        toast("Saved workflow " + clean.name + (loc.kind === "repo" ? " to " + loc.file + ". Commit it to share it." : ". Applied now."));
        var title = m.querySelector(".head h1"); if (title) title.textContent = "Edit workflow " + clean.name; renderPalette();
        refreshOverview(); if (location.hash !== "#/wfedit/" + encodeURIComponent(clean.name)) history.replaceState(null, "", "#/wfedit/" + encodeURIComponent(clean.name));
      }).catch(function (e) { toast(e.message, true); status.textContent = /^not saved/i.test(e.message) ? e.message.replace(/^not saved/, "Not saved") : "Not saved: " + e.message; }).then(function () { save.disabled = !canEdit(); });
    });
    window.addEventListener("beforeunload", function warn(e) { if (!document.body.contains(canvas)) { window.removeEventListener("beforeunload", warn); return; } if (dirty) { e.preventDefault(); e.returnValue = ""; } });

    api("/api/config").then(function (x) {
      c = x;
      if (c.error) { inspector.textContent = "indra.toml has an error; fix it in Settings > File first."; return; }
      var found = original && (c.config.workflow || []).find(function (w) { return w.name === original; });
      if (found) loc = { kind: "config" };
      else if (original) repoFiles().forEach(function (f) { var w = (f.workflows || []).find(function (x) { return x.name === original; }); if (w && !found) { found = w; loc = { kind: "repo", repo: f.repo, file: f.file }; } });
      was = found ? JSON.parse(JSON.stringify(loc)) : null;
      wf = found ? JSON.parse(JSON.stringify(found)) : { name: original || "new-workflow", task: [] };
      wf.task = wf.task || [];
      tidy(false); renderPalette(); draw(); inspect();
      api("/api/defs" + hq()).then(function (d) { skills = d.filter(function (x) { return x.kind === "skill"; }); renderPalette(); if (sel) inspect(); }).catch(function () { /* no skills */ });
      save.disabled = !c.editable; status.textContent = c.editable ? "" : "Read-only: " + c.locked;
    }).catch(function (e) { inspector.textContent = e.status === 404 ? "This console cannot edit indra.toml." : e.message; });
  }

  // ---- settings: indra.toml, edited here ------------------------------------------
  // Each section of the config file has a schema; forms are built from it, and
  // a save replaces that one section on the server, which keeps the rest of
  // the file (comments included), checks the result, and applies it at once.
  var TABS = [["general", "General"], ["profiles", "Profiles"], ["runners", "Runners"], ["nodes", "Nodes"], ["workflows", "Workflows"], ["components", "Saved tasks"], ["accounts", "Accounts"],
    ["presets", "Presets"], ["teams", "Teams"], ["marketplaces", "Marketplaces"], ["harnesses", "Harnesses"], ["connectors", "Connectors"], ["secrets", "Secrets"], ["users", "Users"], ["file", "File"]];

  function opts(list, blank) { return (blank ? [["", blank]] : []).concat(list.map(function (x) { return [x, x]; })); }
  function cfgRunnerNames(c) {
    var n = {}; (c.config.runner || []).concat(c.builtin.runners).forEach(function (r) { n[r.name] = 1; }); return Object.keys(n);
  }
  function cfgHarnessNames(c) {
    var n = {}; (c.config.harness || []).concat(c.builtin.harnesses).forEach(function (x) { n[x.name] = 1; }); return Object.keys(n);
  }
  function cfgNodeNames(c) { return ["local"].concat((c.config.node || []).map(function (n) { return n.name; })); }
  // Profiles from indra.toml, then the built-in ones it does not replace.
  function cfgProfileNames(c) {
    var n = {}; (c.config.profile || []).concat(c.config.builtins === false ? [] : (c.builtin.profiles || [])).forEach(function (p) { n[p.name] = 1; }); return Object.keys(n);
  }
  // Starting points for "Add profile".
  var PROFILE_TEMPLATES = [
    { label: "Planner (Opus, read-only)", v: { name: "planner", description: "Reads and plans; changes nothing", runner: "claude", model: "opus", permission_mode: "dontAsk", effort: "high",
      allowed_tools: ["Read", "Grep", "Glob", "Bash(git log:*)", "Bash(git diff:*)", "Bash(ls:*)"] } },
    { label: "Builder (Haiku)", v: { name: "builder", description: "Carries out small, well-specified steps cheaply", runner: "claude", model: "haiku", permission_mode: "acceptEdits",
      allowed_tools: ["Read", "Grep", "Glob", "Edit", "Write", "Bash(git diff:*)", "Bash(git status:*)"] } },
    { label: "Opus with Haiku subagents", v: { name: "lead", description: "Opus leads; Claude Code's own subagents run on Haiku", runner: "claude", model: "opus", permission_mode: "acceptEdits", subagent_model: "haiku" } },
    { label: "Worker (Sonnet, accept edits)", v: { name: "worker", description: "Does the work the planner laid out", runner: "claude", model: "sonnet", permission_mode: "acceptEdits",
      allowed_tools: ["Read", "Grep", "Glob", "Edit", "Write", "Bash(git diff:*)", "Bash(git status:*)"] } },
    { label: "Local model (Ollama)", v: { name: "local", description: "Claude Code on a local model served by Ollama", runner: "claude", model: "qwen3-coder", provider: "ollama", permission_mode: "manual" } },
    { label: "Opus with Ollama subagents", v: { name: "lead-local", description: "Opus leads on Anthropic; its subagents run on a local model", runner: "claude", model: "opus", permission_mode: "acceptEdits",
      subagent_model: "qwen3-coder", subagent_provider: "ollama" } },
    { label: "Codex on Ollama", v: { name: "codex-local", description: "Codex on a local model served by Ollama", runner: "codex", model: "gpt-oss:20b", provider: "ollama" } }
  ];

  // Starting points for "Add team" (they name profiles from the templates above).
  var TEAM_TEMPLATES = [
    { label: "Opus plans, Haiku builds", v: { name: "opus-haiku", description: "Plan with Opus, build with Haiku, Opus reviews", planner: "planner", worker: "builder", reviewer: "planner" } },
    { label: "Opus plans, local model builds", v: { name: "opus-local", description: "Plan with Opus, build on a self-hosted model through Ollama", planner: "planner", worker: "local", light_worker: "local", reviewer: "planner" } }
  ];

  var SCHEMA = {
    account: { tab: "accounts", noun: "account", fields: function (c) { return [
      { k: "name", label: "Name", req: true, help: "A profile can pin it (Account on the profile); otherwise routing chooses." },
      { k: "description", label: "Description" },
      { k: "agent", label: "Agent", type: "select", options: [["", "Claude Code"], ["codex", "Codex"], ["gemini", "Gemini CLI"]].concat(cfgRunnerNames(c).filter(function (r) { return ["claude", "codex", "gemini"].indexOf(r) < 0; }).map(function (r) { return [r, r + " (runner)"]; })) },
      { k: "credential", label: "Credential", req: true, placeholder: "secret:anthropic-work", help: "A secret reference: add the key in the Secrets tab first. Claude subscription: run `claude setup-token` and store the token; API keys work for every agent. The value goes only to the agent, also on remote nodes, which then need no login of their own." },
      { k: "kind", label: "Kind", type: "select", options: [["", "API key"], ["oauth_token", "Claude subscription token (claude setup-token)"]] },
      { k: "env", label: "Environment variable", placeholder: "by agent: ANTHROPIC_API_KEY, CLAUDE_CODE_OAUTH_TOKEN, OPENAI_API_KEY, GEMINI_API_KEY", help: "Only for other agents, or to override." },
      { k: "daily_usd", label: "Daily limit (USD)", type: "number", placeholder: "none" },
      { k: "monthly_usd", label: "Monthly limit (USD)", type: "number", placeholder: "none" },
      { k: "daily_runs", label: "Daily limit (runs)", type: "number", placeholder: "none", help: "For subscriptions, which report no cost: how many agent runs a day." },
      { k: "nodes", label: "Only on nodes", type: "list", placeholder: "every node", help: "One per line; empty: any node, local included." },
      { k: "priority", label: "Priority", type: "number", placeholder: "0", help: "With in-order routing, lower goes first." },
      { k: "disabled", label: "Turned off (routing skips it)", type: "bool" }]; },
      summary: function (a) { return (a.agent || "claude") + ", " + (a.credential || "") + (a.daily_usd ? ", $" + a.daily_usd + "/day" : "") + (a.monthly_usd ? ", $" + a.monthly_usd + "/month" : "") + (a.daily_runs ? ", " + a.daily_runs + " runs/day" : "") + (a.disabled ? " (off)" : ""); } },
    component: { tab: "components", noun: "saved task", fields: function (c) { return [
      { k: "name", label: "Name", req: true, help: "A task saved for reuse: drag it into any workflow from the editor's palette. A task's own fields override it, and a change here reaches every workflow that uses it." },
      { k: "description", label: "Description" },
      { k: "prompt", label: "Prompt", type: "textarea", help: "{target} and {node} are filled in. With a skill: its arguments (default {target})." },
      { k: "skill", label: "Skill", placeholder: "e.g. sh-security-review", help: "Run this skill instead of a free prompt (optional)." },
      { k: "profile", label: "Profile", type: "select", options: opts(cfgProfileNames(c), "None") },
      { k: "runner", label: "Runner", type: "select", options: opts(cfgRunnerNames(c), "Default") },
      { k: "node", label: "Node", type: "select", options: opts(cfgNodeNames(c), "Default (local)") },
      { k: "target", label: "Target" },
      { k: "retries", label: "Retries", type: "number", placeholder: "0" }]; },
      summary: function (x) { return x.prompt || x.description || ""; } },
    connector: { tab: "connectors", noun: "connector", fields: function (c) { return [
      { k: "name", label: "Name", req: true },
      { k: "kind", label: "Tracker", type: "select", req: true, options: [["jira", "Jira (Cloud)"], ["github", "GitHub Issues"], ["clickup", "ClickUp"], ["linear", "Linear"]] },
      { k: "url", label: "Site URL", placeholder: "https://acme.atlassian.net", help: "Jira: your site. GitHub, ClickUp and Linear: leave empty for the public API (set it for GitHub Enterprise)." },
      { k: "project", label: "Project", req: true, help: "Jira project key (SEC), GitHub owner/repo, ClickUp list id, or Linear team key (ENG)." },
      { k: "query", label: "Filter", help: "Jira: extra JQL (labels = agent). GitHub or Linear: a label. ClickUp: statuses, comma-separated. Imports follow the tracker's pages, up to 500 items." },
      { k: "email", label: "Account email (Jira)" },
      { k: "token", label: "Token", req: true, placeholder: "secret:jira", help: "A secret reference, never the token itself: add the token in the Secrets tab (scope it to connector:<this name>)." },
      { k: "profile", label: "Run with profile", type: "select", options: opts(cfgProfileNames(c), "None") },
      { k: "node", label: "Node", type: "select", options: opts(cfgNodeNames(c), "local") },
      { k: "target", label: "Target", help: "The directory agents work in for these items (default: the harness's first target)." },
      { k: "prompt", label: "Instructions", type: "textarea", help: "Optional. {title}, {key}, {url} and {body} are filled in." },
      { k: "poll_minutes", label: "Import every N minutes", type: "number", placeholder: "only when asked" },
      { k: "auto_run", label: "Start an agent on each new item (within the node's capacity)", type: "bool" },
      { k: "states", label: "States in the tracker", type: "kv", help: "running=In Progress, review=In Review, done=Done, failed=... one per line (GitHub: a label, or open/closed)." }]; },
      summary: function (x) { return x.kind + " " + x.project + (x.query ? " (" + x.query + ")" : "") + (x.auto_run ? ", auto-run" : ""); } },
    profile: { tab: "profiles", noun: "profile", fields: function (c) { return [
      { k: "name", label: "Name", req: true, help: "Pick it on Run an agent, in presets, in workflow steps and on the board." },
      { k: "description", label: "Description" },
      { k: "runner", label: "Runner", type: "select", options: opts(cfgRunnerNames(c), "Whatever Launch has selected") },
      { k: "account", label: "Account", type: "select", options: [["", "Automatic (routing picks, else this machine's login)"], ["login", "The machine's own login"]].concat((c.config.account || []).map(function (a) { return [a.name, a.name]; })),
        help: "Which credentials its agents use. Automatic: the account with the most left (Settings > Accounts), or each machine's own CLI login when there are none." },
      { k: "model", label: "Model", list: "models", placeholder: "default", help: "An alias (opus, sonnet, haiku) or a full model id; for Ollama, the local model's name (the list shows what your Ollama server has)." },
      { k: "provider", label: "Runs on", type: "select", options: [["", "Anthropic, or however the runner is set up"], ["ollama", "Ollama: the whole agent on the model above"]],
        help: "Ollama works with Claude Code (0.14+ serves its API) and Codex (--oss). indra sets the base URL and maps opus, sonnet and haiku to the model above, so agent files that name them run locally too." },
      { k: "ollama_url", label: "Ollama server", placeholder: "http://localhost:11434", help: "Another machine's Ollama, e.g. http://gpu-box:11434. Leave empty for this machine." },
      { k: "permission_mode", label: "Permission mode", type: "select", options: [["", "default"]].concat(MODES) },
      { k: "effort", label: "Effort", type: "select", options: opts(["low", "medium", "high", "xhigh", "max"], "default") },
      { k: "mcp", label: "MCP servers", type: "select", options: [["", "as Claude Code is set up"], ["none", "none: a much smaller context"]], help: "Each MCP server's tools fill the agent's context before it does anything. Choose none for agents that do not need them." },
      { k: "allowed_tools", label: "Always allowed tools", type: "list", help: "One rule per line, e.g. Read, Edit, Bash(git diff:*). These never ask." },
      { k: "disallowed_tools", label: "Never allowed tools", type: "list", help: "One rule per line, e.g. Bash(rm:*), WebFetch." },
      { k: "env", label: "Environment", type: "kv", help: "KEY=value per line, e.g. CLAUDE_CODE_MAX_OUTPUT_TOKENS=8000. For Ollama, use Runs on above instead of setting ANTHROPIC_BASE_URL here. For a token, write KEY=secret:<name> and set it in the Secrets tab: the value never goes in indra.toml." },
      { k: "subagent_model", label: "Model for its subagents", list: "models", placeholder: "same as the agent", help: "Claude Code only (CLAUDE_CODE_SUBAGENT_MODEL): e.g. haiku, so an Opus agent hands its Task subagents to a cheaper model." },
      { k: "subagent_provider", label: "Subagents run on", type: "select", options: [["", "the same place as the agent"], ["ollama", "Ollama (the subagent model above is a local one)"]],
        help: "Claude Code only. The main agent stays on Anthropic and its subagents run locally: indra points Claude Code at its model router, which sends the subagent model to Ollama and everything else to Anthropic. Your API key or login never goes to Ollama." },
      { k: "memory", label: "Start with the memory notes most relevant to the prompt", type: "bool" },
      { k: "reflect", label: "Before finishing, propose learnings for review (Claude Code)", type: "bool" }]; },
      summary: function (p) { return [p.runner || "any runner", (p.model || "default model") + (p.provider === "ollama" ? " on Ollama" : ""), p.permission_mode || "default mode"].join(", ")
        + (p.subagent_model ? ", subagents: " + p.subagent_model + (p.subagent_provider === "ollama" ? " on Ollama" : "") : "") + (p.env && Object.keys(p.env).length ? ", env: " + Object.keys(p.env).join(" ") : "") + (p.memory ? ", memory" : "") + (p.reflect ? ", reflects" : ""); } },
    runner: { tab: "runners", noun: "runner", fields: function () { return [
      { k: "name", label: "Name", req: true, help: "How you pick it on Launch and in workflows. A built-in name (claude, codex, gemini) replaces the built-in." },
      { k: "command", label: "Program", req: true, help: "A program on PATH or a full path. It runs directly, never through a shell." },
      { k: "args", label: "Arguments", type: "list", req: true, help: "One per line. {prompt} is required; {target} is the directory the agent runs in. Each is filled in as one whole argument." },
      { k: "format", label: "Output", type: "select", options: [["", "Plain text (default)"], ["text", "Plain text"], ["claude-stream-json", "Claude Code stream-json: subagents, tool calls, cost"]] },
      { k: "resume_args", label: "Arguments to continue a session", type: "list", help: "Used by Pivot > Continue; {session} is the session id. stream-json runners default to --resume {session}." },
      { k: "fork_args", label: "Arguments to branch a session", type: "list", help: "Added after those when the original agent keeps running, e.g. --fork-session." },
      { k: "model_args", label: "Arguments to pick a model", type: "list", placeholder: "-m\n{model}", help: "One per line, with {model} in one of them, e.g. -m then {model}. Without them, a profile or task cannot choose this runner's model (Claude Code does not need them)." }]; },
      summary: function (r) { return r.command + " " + (r.args || []).join(" "); } },
    node: { tab: "nodes", noun: "node", fields: function () { return [
      { k: "name", label: "Name", req: true },
      { k: "description", label: "Description" },
      { k: "ssh", label: "SSH destination", help: "host, user@host, or a Host alias from ~/.ssh/config. `ssh <destination>` must work without a password prompt, and the host key must already be trusted. Leave it empty for a node that dials in to this hub (indra node connect) and has no SSH." },
      { k: "targets", label: "Targets", type: "list", help: "Codebases on that machine, one path per line, as paths there." },
      { k: "max_parallel", label: "Agents at once", type: "number", placeholder: "2" },
      { k: "port", label: "SSH port", type: "number", placeholder: "22" },
      { k: "identity", label: "Private key file", placeholder: "whatever ssh would use" },
      { k: "indra", label: "indra on the remote", placeholder: "indra" },
      { k: "args", label: "Extra arguments for the remote indra", type: "list", help: "One per line, e.g. --config and /etc/indra.toml." },
      { k: "ssh_command", label: "ssh client", placeholder: "ssh" }]; },
      summary: function (n) { return (n.ssh || "dials in (no SSH)") + "  -  " + (n.targets || []).length + " targets, " + (n.max_parallel || 2) + " agents at once"; } },
    marketplace: { tab: "marketplaces", noun: "marketplace", fields: function () { return [
      { k: "name", label: "Name", req: true, help: "A built-in name (indra, anthropic-skills, claude-plugins-official) replaces that built-in." },
      { k: "source", label: "Source", req: true, placeholder: "owner/repo", help: "owner/repo on GitHub, an https git URL, or a folder on this machine." },
      { k: "ref", label: "Branch, tag or commit", placeholder: "the default branch" },
      { k: "description", label: "Description" }]; },
      summary: function (x) { return x.source + (x.ref ? "@" + x.ref : ""); } },
    team: { tab: "teams", noun: "team", fields: function (c) { var P = cfgProfileNames(c); return [
      { k: "name", label: "Name", req: true, help: "Pick it under Launch > Plan and delegate." },
      { k: "description", label: "Description" },
      { k: "planner", label: "Planner", type: "select", req: true, options: opts(P), help: "Reads the code and writes the plan; give it a strong model and read-only tools (see the Planner profile template)." },
      { k: "worker", label: "Worker", type: "select", req: true, options: opts(P), help: "Carries out each step: Haiku, a local model through Ollama, a small Codex model..." },
      { k: "light_worker", label: "Light steps", type: "select", options: opts(P, "Same as the worker"), help: "For mechanical steps (renames, docs, boilerplate): the cheapest model." },
      { k: "reviewer", label: "Reviewer", type: "select", options: opts(P, "No review step"), help: "Checks the result against the plan at the end and fixes small mistakes." },
      { k: "max_steps", label: "Most steps", type: "number", placeholder: "8" },
      { k: "auto_run", label: "Run the plan without waiting for my review", type: "bool" },
      { k: "parallel", label: "Let independent steps run at the same time (they share the directory)", type: "bool" }]; },
      summary: function (t) { return t.planner + " plans, " + t.worker + " builds" + (t.light_worker ? ", " + t.light_worker + " for light steps" : "") + (t.reviewer ? ", " + t.reviewer + " reviews" : "") + (t.auto_run ? "; runs without review" : ""); } },
    preset: { tab: "presets", noun: "preset", fields: function (c) { return [
      { k: "name", label: "Name", req: true },
      { k: "prompt", label: "Prompt", type: "textarea", req: true, help: "{target} becomes the target path, {node} the node name." },
      { k: "harness", label: "Only for harness", type: "select", options: opts(cfgHarnessNames(c), "Any harness") },
      { k: "runner", label: "Runner", type: "select", options: opts(cfgRunnerNames(c), "Whatever Launch has selected") },
      { k: "profile", label: "Profile", type: "select", options: opts(cfgProfileNames(c), "None") }]; },
      summary: function (p) { return p.prompt; } },
    workflow: { tab: "workflows", noun: "workflow", fields: function (c) { return [
      { k: "name", label: "Name", req: true, help: "Run it from the Workflows page or with `indra run <name>`." },
      { k: "description", label: "Description" },
      { k: "harness", label: "Harness", type: "select", options: opts(cfgHarnessNames(c), "The first one") },
      { k: "task", label: "Tasks", type: "rows", noun: "task", fields: [
        { k: "id", label: "Id", req: true },
        { k: "node", label: "Node", type: "select", options: opts(cfgNodeNames(c)), dflt: "local" },
        { k: "runner", label: "Runner", type: "select", options: opts(cfgRunnerNames(c)), dflt: "claude" },
        { k: "profile", label: "Profile", type: "select", options: opts(cfgProfileNames(c), "None"), help: "Model, permission mode and the rest. Its runner, if it names one, replaces the runner above." },
        { k: "model", label: "Model", list: "models", placeholder: "the profile's", help: "This task's model, over the profile's; with Ollama, a local model's name." },
        { k: "provider", label: "Runs on", type: "select", options: [["", "the profile's choice"], ["anthropic", "Anthropic"], ["ollama", "Ollama (local model)"]] },
        { k: "component", label: "Saved task", type: "select", options: opts((c.config.component || []).map(function (x) { return x.name; }), "None"), help: "Start from a reusable component; fields left empty come from it." },
        { k: "workflow", label: "Runs workflow", type: "select", options: opts((c.config.workflow || []).map(function (x) { return x.name; }), "None (a task)"), help: "Run another workflow as this one step instead of a task." },
        { k: "target", label: "Target", help: "A directory on that node (or from the component). Local paths are relative to the config file." },
        { k: "prompt", label: "Prompt", type: "textarea", help: "{target} and {node} are filled in (or the component's prompt)." },
        { k: "needs", label: "Runs after", type: "words", help: "Task ids, separated by spaces or commas. It runs only if they all succeed." },
        { k: "retries", label: "Retries", type: "number", placeholder: "0" }] }]; },
      summary: function (w) { return (w.task || []).map(function (t) { return t.id + "@" + (t.node || "local"); }).join(", "); } },
    harness: { tab: "harnesses", noun: "harness", fields: function () { return [
      { k: "name", label: "Name", req: true, help: "A built-in name (indra, claude-code) replaces the built-in profile." },
      { k: "description", label: "Description" },
      { k: "root", label: "Root", placeholder: ".", help: "Base folder for the globs below, relative to the config file." },
      { k: "agents", label: "Agent files", type: "list", help: "Globs, one per line, e.g. agents/*.md" },
      { k: "skills", label: "Skill files", type: "list", help: "e.g. skills/*/SKILL.md" },
      { k: "prompts", label: "Prompt files", type: "list", help: "e.g. prompts/**/*.md" },
      { k: "memory", label: "Memory vaults", type: "list", help: "{target} expands to each target, e.g. {target}/.memory" },
      { k: "targets", label: "Targets", type: "list", help: "Codebases whose runs are shown and where agents run. --target on the command line overrides these." },
      { k: "runs", label: "Run directories", placeholder: ".runs/*", help: "A glob under each target, one directory per run." },
      { k: "findings", label: "Results files", type: "list", help: "JSONL files inside a run, e.g. results.jsonl" },
      { k: "group_by", label: "Group results by", placeholder: "agent" },
      { k: "stages", label: "Stages", type: "rows", noun: "stage", fields: [
        { k: "name", label: "Name", req: true }, { k: "agent", label: "Agent" }, { k: "done_when", label: "Done when this file exists" }] }]; },
      summary: function (x) { return x.description || x.root || "."; } }
  };

  // One form control per field; get() returns the value to send, or undefined to leave it out.
  function control(f, val) {
    var el, get;
    var ph = f.placeholder || "";
    if (f.type === "textarea") {
      el = h("textarea", { placeholder: ph }); el.value = val || "";
      get = function () { return el.value.trim() ? el.value : (f.req ? "" : undefined); };
    } else if (f.type === "list") {
      el = h("textarea", { class: "list", rows: "3", placeholder: ph }); el.value = (val || []).join("\n");
      get = function () { var v = el.value.split("\n").map(function (x) { return x.trim(); }).filter(Boolean); return v.length || f.req ? v : undefined; };
    } else if (f.type === "words") {
      el = h("input", { type: "text", placeholder: ph }); el.value = (val || []).join(", ");
      get = function () { var v = el.value.split(/[\s,]+/).filter(Boolean); return v.length ? v : undefined; };
    } else if (f.type === "number") {
      el = h("input", { type: "number", min: "0", step: "1", placeholder: ph }); el.value = val == null ? "" : String(val);
      get = function () { return el.value === "" ? undefined : Number(el.value); };
    } else if (f.type === "bool") {
      el = h("input", { type: "checkbox" }); el.checked = !!val;
      get = function () { return el.checked; };
    } else if (f.type === "select") {
      el = h("select", null, f.options.map(function (o) { return h("option", { value: o[0], text: o[1] }); }));
      var v = val == null ? (f.dflt || "") : val;
      if (v && !f.options.some(function (o) { return o[0] === v; })) el.appendChild(h("option", { value: v, text: v + " (not defined here)" }));
      el.value = v;
      get = function () { return el.value || undefined; };
    } else if (f.type === "rows") {
      return rowsControl(f, val || []);
    } else if (f.type === "kv") {
      el = h("textarea", { class: "list", rows: "3", placeholder: ph || "KEY=value" });
      el.value = Object.keys(val || {}).map(function (k) { return k + "=" + val[k]; }).join("\n");
      get = function () {
        var o = {}, any = false;
        el.value.split("\n").forEach(function (line) {
          var i = line.indexOf("="); if (i <= 0) return;
          o[line.slice(0, i).trim()] = line.slice(i + 1); any = true;
        });
        return any ? o : undefined;
      };
    } else {
      el = h("input", { type: "text", placeholder: ph, list: f.list || null }); el.value = val == null ? "" : String(val);
      get = function () { return el.value.trim() ? el.value.trim() : (f.req ? "" : undefined); };
    }
    if (f.req) el.setAttribute("required", "");
    var label = f.type === "bool"
      ? h("label", { class: "check" }, el, h("span", null, f.label, f.help ? h("small", { text: " " + f.help }) : null))
      : h("label", null, h("span", null, f.label, f.req ? h("b", { class: "req", text: " *" }) : null), f.help ? h("small", { text: f.help }) : null, el);
    return { el: label, get: get };
  }

  function rowsControl(f, items) {
    var list = h("div", { class: "rows" }), rows = [];
    function add(item) {
      var ctl = f.fields.map(function (sf) { return { k: sf.k, c: control(sf, item[sf.k]) }; });
      var box = h("fieldset", { class: "rowbox" });
      var row = { ctl: ctl, box: box, item: item };
      box.appendChild(h("legend", null, f.noun + " " + (rows.length + 1)));
      ctl.forEach(function (x) { box.appendChild(x.c.el); });
      box.appendChild(h("div", { class: "row" }, h("button", { type: "button", class: "ghost danger", onclick: function () {
        rows.splice(rows.indexOf(row), 1); box.remove();
        rows.forEach(function (r, i) { r.box.querySelector("legend").textContent = f.noun + " " + (i + 1); });
      } }, "Remove " + f.noun)));
      rows.push(row); list.appendChild(box);
    }
    items.forEach(add);
    var wrap = h("div", { class: "rowsfield" }, h("span", { class: "flabel", text: f.label }), list,
      h("button", { type: "button", onclick: function () { add({}); } }, "Add " + f.noun));
    return { el: wrap, get: function () {
      // Keys this form does not show (a task's component, sub-workflow or
      // position, say) are kept as they were.
      var out = rows.map(function (r) { var o = keepUnshown(r.item, r.ctl); r.ctl.forEach(function (x) { var v = x.c.get(); if (v !== undefined) o[x.k] = v; }); return o; });
      return out.length ? out : undefined;
    } };
  }

  // A copy of `item` without the keys a form shows: what a save must keep.
  function keepUnshown(item, ctl) {
    var o = {}, shown = {};
    ctl.forEach(function (x) { shown[x.k] = true; });
    Object.keys(item || {}).forEach(function (k) { if (!shown[k]) o[k] = item[k]; });
    return o;
  }

  function itemForm(fields, item, title, onSave, onCancel) {
    var ctl = fields.map(function (f) { return { k: f.k, c: control(f, item[f.k]) }; });
    var err = h("div", { class: "note bad", hidden: true, role: "alert" });
    var save = h("button", { class: "primary", type: "submit" }, "Save");
    var form = h("form", { class: "form card cfgform", onsubmit: function (e) {
      e.preventDefault();
      var o = keepUnshown(item, ctl); ctl.forEach(function (x) { var v = x.c.get(); if (v !== undefined) o[x.k] = v; });
      save.disabled = true; err.hidden = true;
      onSave(o).catch(function (ex) { err.textContent = ex.message; err.hidden = false; save.disabled = false; });
    } }, h("h2", { text: title }), ctl.map(function (x) { return x.c.el; }), err,
      h("div", { class: "row" }, save, h("button", { type: "button", onclick: onCancel }, "Cancel")));
    return form;
  }

  function pageSettings(m, tab) {
    tab = TABS.some(function (t) { return t[0] === tab; }) ? tab : "general";
    m.appendChild(header("Settings", "Everything in indra.toml, edited here. Each save is checked first and applies at once; agents already running are not affected."));
    var mg = S.ov.managed || {};
    if (mg.error) m.appendChild(h("div", { class: "note bad", role: "alert", text: "Your organization's settings could not be read, so they are not applied: " + mg.error }));
    else if ((mg.enforced || []).length) m.appendChild(h("div", { class: "note" }, h("b", { text: "Set by your organization: " }), mg.enforced.join(", "),
      h("span", { class: "muted small", text: ". These come from " + mg.dir + " (managed.toml) and override what you set here." })));
    else if (mg.defaults) m.appendChild(h("p", { class: "muted small", text: "Your organization's defaults (" + mg.dir + ", defaults.toml) apply where you set nothing else." }));
    var tabs = h("nav", { class: "tabs", "aria-label": "Settings sections" }, TABS.map(function (t) {
      return h("a", { href: "#/settings/" + t[0], class: t[0] === tab ? "on" : "", "aria-current": t[0] === tab ? "page" : null, text: t[1] });
    }));
    var info = h("div"), body = h("div");
    m.appendChild(tabs); m.appendChild(info); m.appendChild(body);
    var c = null;

    function load() { return api("/api/config").then(function (x) { c = x; render(); }); }
    function save(payload, what) {
      payload.version = c.version;
      return api("/api/config", { method: "PUT", body: payload }).then(function (x) {
        c = x; toast((what || "Saved") + ". Applied now."); refreshOverview(); render();
      }).catch(function (e) {
        if (e.status === 409) { toast("The file changed outside this page; reloaded it.", true); load(); }
        throw e;
      });
    }
    function saveSection(section, value, what) { return save({ section: section, value: value }, what); }

    function render() {
      info.textContent = ""; body.textContent = "";
      info.appendChild(h("p", { class: "muted small cfgpath" }, "File: ", h("code", { text: c.path }),
        c.exists ? null : " (created when you first save)"));
      if (!c.editable) info.appendChild(h("div", { class: "note warn", text: "Settings are read-only here: " + c.locked + "." }));
      if (c.error && tab !== "file") {
        body.appendChild(h("div", { class: "note bad" }, "The file has an error, so the forms are off until it is fixed in the ", h("a", { href: "#/settings/file", text: "File" }), " tab: " + c.error));
        return;
      }
      ({ general: general, profiles: function () { list("profile", c.config.builtins === false ? [] : c.builtin.profiles); }, runners: function () { list("runner", c.builtin.runners); }, nodes: function () { list("node"); },
        workflows: function () { list("workflow"); }, presets: function () { list("preset", c.builtin.presets); },
        harnesses: function () { list("harness", c.builtin.harnesses); }, connectors: function () { list("connector"); }, components: function () { list("component"); }, accounts: function () { list("account"); }, teams: function () { list("team", c.config.builtins === false ? [] : c.builtin.teams); }, marketplaces: function () { list("marketplace"); }, secrets: secrets, users: usersTab, file: file })[tab]();
    }

    function general() {
      var cf = c.config, fields = [
        { k: "builtins", label: "Use the built-ins: harness profiles (the indra plugin, Claude Code projects), agent profiles (planner, worker, builder, reviewer) and the plan-and-build team", type: "bool" },
        { k: "max_parallel", label: "Agents at once on this machine", type: "number", placeholder: "4", help: "Workflows and fan-outs wait for room; a manual Launch always starts." },
        { k: "bind", label: "Listen on", placeholder: "127.0.0.1:7878", help: "Used the next time indra serve starts (now: " + c.startup.bind + "). Keep it on loopback." },
        { k: "read_only", label: "Start read-only (no launching, no editing)", type: "bool", help: "Used the next time indra serve starts." },
        { k: "webrtc", label: "Let nodes that dial in use a direct WebRTC channel (else everything goes over their WebSocket)", type: "bool" },
        { k: "ice_servers", label: "STUN servers for WebRTC", type: "list", placeholder: "stun:stun.l.google.com:19302", help: "One URL per line. TURN servers with a username and credential go in indra.toml as [[server.ice]] (see the nodes guide)." },
        { k: "strategy", label: "Accounts: routing", type: "select", options: [["", "Most left (default)"], ["round_robin", "Round robin"], ["in_order", "In order (by priority)"]], help: "How a launch picks among the accounts for its agent (Settings > Accounts)." },
        { k: "fallback_to_login", label: "Accounts: when all are used up, use the machine's own login", type: "bool" },
        { k: "cooldown_minutes", label: "Accounts: rest after a usage limit (minutes)", type: "number", placeholder: "60", help: "When the agent does not say when its limit resets." },
        { k: "shell", label: "Terminal: shell", placeholder: "your login shell ($SHELL)", help: "What a browser terminal starts, e.g. /bin/zsh, fish or pwsh. Installed: " + ((S.ov && S.ov.shells) || []).join(", ") + "." },
        { k: "shell_args", label: "Terminal: shell arguments", type: "list", placeholder: "-l", help: "One per line, e.g. -l for a login shell." },
        { k: "shells", label: "Terminal: more shells to offer", type: "list", placeholder: "/opt/homebrew/bin/fish", help: "Besides /etc/shells; the terminal's New form lists them." }];
      var term = cf.terminal || {}, rt = cf.routing || {};
      var vals = { builtins: cf.builtins !== false, max_parallel: cf.local.max_parallel, bind: cf.server.bind, read_only: !!cf.server.read_only,
        webrtc: cf.server.webrtc !== false, ice_servers: cf.server.ice_servers, shell: term.shell, shell_args: term.args, shells: term.shells,
        strategy: rt.strategy, fallback_to_login: rt.fallback_to_login !== false, cooldown_minutes: rt.cooldown_minutes };
      var form = itemForm(fields, vals, "General", function (o) {
        // One section at a time; each save returns the version the next one needs.
        return saveSection("builtins", o.builtins ? null : false, "Saved")
          // Each section keeps what this form does not show.
          .then(function () { var lc = Object.assign({}, cf.local); delete lc.max_parallel; if (o.max_parallel !== undefined) lc.max_parallel = o.max_parallel; return saveSection("local", lc); })
          .then(function () {
            var sv = Object.assign({}, cf.server); ["bind", "read_only", "webrtc", "ice_servers"].forEach(function (k) { delete sv[k]; });
            if (o.bind) sv.bind = o.bind; if (o.read_only) sv.read_only = true; if (!o.webrtc) sv.webrtc = false;
            if (o.ice_servers && o.ice_servers.length) sv.ice_servers = o.ice_servers;
            return saveSection("server", sv);
          })
          .then(function () {
            var t = {}; if (o.shell) t.shell = o.shell; if (o.shell_args && o.shell_args.length) t.args = o.shell_args; if (o.shells && o.shells.length) t.shells = o.shells;
            return saveSection("terminal", Object.keys(t).length ? t : null);
          })
          .then(function () {
            var r = {}; if (o.strategy) r.strategy = o.strategy; if (!o.fallback_to_login) r.fallback_to_login = false; if (o.cooldown_minutes) r.cooldown_minutes = o.cooldown_minutes;
            return saveSection("routing", Object.keys(r).length ? r : null);
          });
      }, function () { render(); });
      if (!c.editable) Array.prototype.forEach.call(form.querySelectorAll("input,select,textarea,button"), function (x) { x.disabled = true; });
      body.appendChild(form);
      if (c.startup.targets.length) body.appendChild(h("div", { class: "note", text: "This console was started with --target " + c.startup.targets.join(", ") + ", which overrides every harness's targets until it restarts." }));
    }

    function list(section, builtins) {
      var d = SCHEMA[section], items = (c.config[section] || []).slice();
      var names = items.map(function (x) { return x.name; });
      function edit(i, item, title) {
        body.textContent = "";
        body.appendChild(itemForm(d.fields(c), item, title, function (o) {
          var next = items.slice();
          if (i === null) next.push(o); else next[i] = o;
          return saveSection(section, next, "Saved " + d.noun + " " + o.name);
        }, render));
        var first = body.querySelector("input,textarea,select"); if (first) first.focus();
      }
      var add = h("button", { class: "primary", type: "button", disabled: !c.editable, onclick: function () {
        // A new team starts from the built-in profiles, so its selects are never blank.
        edit(null, section === "team" ? { planner: "planner", worker: "worker" } : {}, "New " + d.noun); } }, "Add " + d.noun);
      var exportBtn = h("button", { type: "button", disabled: !items.length, onclick: function () { jsonFile(section + "s.json", items); } }, "Export");
      var importBtn = h("button", { type: "button", disabled: !c.editable, onclick: function () {
        pickFiles({ accept: ".json,application/json" }).then(function (fs) {
          if (!fs.length) return;
          var got; try { got = listFrom(fs[0].text); } catch (e) { toast(fs[0].name + ": " + e.message, true); return; }
          got = got.filter(function (x) { return x && typeof x === "object" && x.name; });
          if (!got.length) { toast("No " + d.noun + " with a name in " + fs[0].name, true); return; }
          var clash = got.filter(function (x) { return names.indexOf(x.name) >= 0; }).map(function (x) { return x.name; });
          if (clash.length && !window.confirm("Replace " + clash.length + " existing " + d.noun + "(s) with the same name (" + clash.join(", ") + ")?")) return;
          var next = items.slice();
          got.forEach(function (x) { var j = names.indexOf(x.name); if (j >= 0) next[j] = x; else next.push(x); });
          saveSection(section, next, "Imported " + got.length + " " + d.noun + "(s)").catch(function (e) { toast(e.message, true); });
        });
      } }, "Import");
      var templates = section === "profile" ? PROFILE_TEMPLATES : section === "team" ? TEAM_TEMPLATES : [];
      body.appendChild(h("div", { class: "row cfgbar" }, add, templates.map(function (t) {
        return h("button", { type: "button", disabled: !c.editable, onclick: function () { edit(null, JSON.parse(JSON.stringify(t.v)), "New " + d.noun + ": " + t.label); } }, "+ " + t.label);
      }), exportBtn, importBtn));
      if (section === "team") body.appendChild(h("p", { class: "muted", text: "A team plans with one profile and builds with cheaper ones: Launch > Plan and delegate. plan-and-build is built in and uses the built-in profiles, so it works with no setup. Any runner can fill any role, for example a Codex planner with a local Ollama builder." }));
      if (section === "profile") body.appendChild(h("p", { class: "muted", text: "A profile is how an agent runs: plan with a strong model, do the work with the one you choose, or try it on a local model. planner, worker, builder and reviewer are built in (Claude Code with Opus, Sonnet and Haiku); Customize one to change it. Launch, presets, workflow tasks and fan-outs can pick one; model and mode can still change while the agent runs." }));
      body.appendChild(modelList() || h("span"));
      var grid = h("div", { class: "grid" });
      items.forEach(function (it, i) {
        var inst = section === "runner" ? c.installed.find(function (r) { return r.name === it.name; }) : null;
        grid.appendChild(h("div", { class: "card cfgitem" },
          h("div", { class: "row" }, h("h4", { text: it.name }),
            inst ? h("span", { class: "pill " + (inst.available ? "ok" : "warn"), text: inst.available ? "installed" : "not found on PATH" }) : null,
            builtins && builtins.some(function (b) { return b.name === it.name; }) ? h("span", { class: "pill", text: "replaces built-in" }) : null),
          h("p", { class: "muted clip", text: d.summary(it) }),
          h("div", { class: "row" },
            section === "workflow" ? h("a", { class: "btn", href: "#/wfedit/" + encodeURIComponent(it.name), text: "Edit graph" }) : null,
            h("button", { type: "button", disabled: !c.editable, onclick: function () { edit(i, it, "Edit " + d.noun + " " + it.name); } }, section === "workflow" ? "Edit as form" : "Edit"),
            h("button", { type: "button", disabled: !c.editable, onclick: function () {
              var copy = JSON.parse(JSON.stringify(it)); copy.name = it.name + "-copy"; edit(null, copy, "New " + d.noun);
            } }, "Duplicate"),
            h("button", { type: "button", class: "danger", disabled: !c.editable, onclick: function () {
              if (!window.confirm("Delete " + d.noun + " " + it.name + "?")) return;
              saveSection(section, items.filter(function (_, j) { return j !== i; }), "Deleted " + it.name).catch(function (e) { toast(e.message, true); });
            } }, "Delete"))));
      });
      (builtins || []).filter(function (b) { return names.indexOf(b.name) < 0; }).forEach(function (b) {
        var inst = section === "runner" ? c.installed.find(function (r) { return r.name === b.name; }) : null;
        grid.appendChild(h("div", { class: "card cfgitem builtin" },
          h("div", { class: "row" }, h("h4", { text: b.name }), h("span", { class: "pill", text: "built in" }),
            inst ? h("span", { class: "pill " + (inst.available ? "ok" : ""), text: inst.available ? "installed" : "not installed" }) : null),
          h("p", { class: "muted clip", text: d.summary(b) }),
          h("div", { class: "row" }, h("button", { type: "button", disabled: !c.editable, onclick: function () {
            edit(null, JSON.parse(JSON.stringify(b)), "Customize " + d.noun + " " + b.name);
          } }, "Customize"))));
      });
      if (!grid.childNodes.length) grid.appendChild(h("div", { class: "card" }, h("p", { class: "muted", text: "No " + d.noun + "s yet." })));
      body.appendChild(grid);
    }

    // Secrets live in an encrypted store, not in indra.toml: this tab talks to /api/secrets.
    // Who may sign in, and what each may do.
    function usersTab() {
      var box = h("div"); body.appendChild(box);
      var ROLES = [["viewer", "viewer: reads everything but settings, secrets and the terminal"], ["operator", "operator: also runs, steers and stops agents and workflows, triages, publishes and posts"], ["admin", "admin: also settings, secrets, users, the terminal, installs and machines"]];
      function load() {
        api("/api/users").then(function (list) {
          box.textContent = "";
          box.appendChild(h("p", { class: "muted", text: list.length
            ? "People sign in with a name and password; each request is allowed by role. The access token " + "indra serve" + " prints is now a key for tools only (indra attach, scripts), not for signing in."
            : "No users yet: whoever has the access token can do everything. Add the first user (always an admin) to switch the console to sign-in with roles; this browser will then ask you to sign in." }));
          if (list.length) box.appendChild(h("div", { class: "tablewrap" }, h("table", null,
            h("thead", null, h("tr", null, ["User", "Role", "", ""].map(function (x) { return h("th", { text: x }); }))),
            h("tbody", null, list.map(function (u) {
              var role = h("select", { "aria-label": "Role of " + u.name }, ROLES.map(function (r) { return h("option", { value: r[0], text: r[0] }); }));
              role.value = u.role;
              role.addEventListener("change", function () {
                api("/api/users/" + encodeURIComponent(u.name), { method: "PATCH", body: { role: role.value } }).then(function () { toast(u.name + " is now " + role.value); load(); })
                  .catch(function (e) { toast(e.message, true); role.value = u.role; });
              });
              return h("tr", null, h("td", null, h("b", { text: u.name })), h("td", null, role),
                h("td", null, h("button", { type: "button", onclick: function () {
                  ask("Set a password for " + u.name, [
                    { name: "p", label: "New password", type: "password", min: 10, hint: "At least 10 characters. Their sessions end." },
                    { name: "again", label: "Again", type: "password", min: 10 }
                  ], "Set password", function (v) {
                    if (v.p !== v.again) throw new Error("The two passwords differ.");
                    return api("/api/users/" + encodeURIComponent(u.name), { method: "PATCH", body: { password: v.p } }).then(function () { toast("Password set for " + u.name); });
                  });
                } }, "Set password")),
                h("td", null, h("button", { type: "button", class: "danger", onclick: function () {
                  if (!window.confirm("Remove " + u.name + "? They are signed out.")) return;
                  api("/api/users/" + encodeURIComponent(u.name), { method: "DELETE" }).then(function () { toast("Removed " + u.name); load(); }).catch(function (e) { toast(e.message, true); });
                } }, "Remove")));
            })))));
          var n = h("input", { type: "text", required: true, placeholder: "alice", autocomplete: "off", "aria-label": "New user name" });
          var r = h("select", { "aria-label": "New user role" }, ROLES.map(function (x) { return h("option", { value: x[0], text: x[1] }); })); r.value = list.length ? "operator" : "admin"; r.disabled = !list.length;
          var p = h("input", { type: "password", required: true, autocomplete: "new-password", placeholder: "at least 10 characters", "aria-label": "New user password" });
          box.appendChild(h("form", { class: "form card", onsubmit: function (e) {
            e.preventDefault();
            api("/api/users", { method: "POST", body: { name: n.value.trim(), role: r.value, password: p.value } }).then(function () {
              toast("Added " + n.value.trim()); if (!list.length) { setTimeout(function () { location.reload(); }, 900); } else load();
            }).catch(function (err) { toast(err.message, true); });
          } }, h("h3", { text: list.length ? "Add a user" : "Add the first user (an admin)" }),
            h("div", { class: "fields" }, h("label", null, "Name", n), h("label", null, "Role", r), h("label", null, "Password", p)),
            h("div", { class: "row" }, h("button", { type: "submit", class: "primary" }, "Add"))));
        }).catch(function (e) { box.textContent = ""; box.appendChild(empty(e.message)); });
      }
      load();
    }

    function secrets() {
      var box = h("div", { class: "muted", text: "Loading secrets..." });
      body.appendChild(h("p", { class: "muted", text: "Tokens and keys for agents, encrypted on this machine and never shown again once saved. A profile uses one through its environment, as KEY=secret:<name>; the agent gets the value when it starts, only if the secret's scope allows that profile and node, and its output shows [secret:<name>] instead." }));
      body.appendChild(box);
      function form(sec) {
        var isNew = !sec;
        sec = sec || { profiles: [], nodes: [] };
        var name = h("input", { type: "text", placeholder: "jira", value: sec.name || "", required: isNew });
        if (!isNew) name.readOnly = true;
        var value = h("input", { type: "password", autocomplete: "new-password", placeholder: isNew ? "the value" : "leave empty to keep the current value" });
        var desc = h("input", { type: "text", value: sec.description || "" });
        var profs = h("textarea", { class: "list", rows: "2", placeholder: "any profile" }); profs.value = (sec.profiles || []).join("\n");
        var nodesF = h("textarea", { class: "list", rows: "2", placeholder: "any node" }); nodesF.value = (sec.nodes || []).join("\n");
        var err = h("div", { class: "note bad", hidden: true, role: "alert" });
        var lines = function (t) { return t.value.split("\n").map(function (x) { return x.trim(); }).filter(Boolean); };
        var f = h("form", { class: "form card cfgform", onsubmit: function (e) {
          e.preventDefault();
          var body2 = { description: desc.value, profiles: lines(profs), nodes: lines(nodesF) };
          if (value.value) body2.value = value.value;
          api("/api/secrets/" + encodeURIComponent(name.value.trim()), { method: "PUT", body: body2 })
            .then(function () { value.value = ""; toast("Saved secret " + name.value.trim()); render(); })
            .catch(function (ex) { err.textContent = ex.message; err.hidden = false; });
        } }, h("h2", { text: isNew ? "New secret" : "Secret " + sec.name }),
          h("label", null, "Name", h("small", { text: "Referenced as secret:<name>." }), name),
          h("label", null, "Value", h("small", { text: "Write-only: it is encrypted and never shown again." }), value),
          h("label", null, "Description", desc),
          h("label", null, "Only these profiles", h("small", { text: "One per line; empty means any profile." }), profs),
          h("label", null, "Only these nodes", h("small", { text: "One per line (local is this machine); empty means any node." }), nodesF),
          err, h("div", { class: "row" }, h("button", { class: "primary", type: "submit" }, "Save"), h("button", { type: "button", onclick: render }, "Cancel")));
        body.textContent = ""; body.appendChild(f); name.focus();
      }
      api("/api/secrets").then(function (r) {
        box.textContent = ""; box.className = "";
        if (!r.editable) box.appendChild(h("div", { class: "note warn", text: "Secrets are read-only here: " + r.locked + "." }));
        box.appendChild(h("div", { class: "row cfgbar" }, h("button", { class: "primary", type: "button", disabled: !r.editable, onclick: function () { form(null); } }, "Add secret")));
        var grid = h("div", { class: "grid" });
        r.secrets.forEach(function (sec) {
          var scope = (sec.profiles.length ? "profiles " + sec.profiles.join(", ") : "any profile") + "; " + (sec.nodes.length ? "nodes " + sec.nodes.join(", ") : "any node");
          grid.appendChild(h("div", { class: "card cfgitem" },
            h("div", { class: "row" }, h("h4", { text: sec.name }), h("span", { class: "pill", text: "secret:" + sec.name })),
            sec.description ? h("p", { class: "muted", text: sec.description }) : null,
            h("p", { class: "muted small", text: scope }),
            h("p", { class: "muted small", text: "Used " + sec.uses + " times" + (sec.last_used ? ", last " + ago(sec.last_used) : "") + ". Updated " + ago(sec.updated) + "." }),
            h("div", { class: "row" },
              h("button", { type: "button", disabled: !r.editable, onclick: function () { form(sec); } }, "Change value or scope"),
              h("button", { type: "button", class: "danger", disabled: !r.editable, onclick: function () {
                if (!window.confirm("Delete secret " + sec.name + "? Profiles that use it will fail to launch.")) return;
                api("/api/secrets/" + encodeURIComponent(sec.name), { method: "DELETE" }).then(function () { toast("Deleted " + sec.name); render(); }).catch(function (e) { toast(e.message, true); });
              } }, "Delete"))));
        });
        if (!r.secrets.length) grid.appendChild(h("div", { class: "card" }, h("p", { class: "muted", text: "No secrets yet." })));
        box.appendChild(grid);
      }).catch(function (e) { box.textContent = e.message; });
    }

    function file() {
      var ta = h("textarea", { class: "code", spellcheck: "false", "aria-label": "indra.toml" }); ta.value = c.text;
      var out = h("div", { class: "note", hidden: true, role: "status" });
      function show(msg, bad) { out.textContent = msg; out.className = "note" + (bad ? " bad" : " ok"); out.hidden = false; }
      var check = h("button", { type: "button", onclick: function () {
        api("/api/config/check", { method: "POST", body: { text: ta.value } })
          .then(function (r) { show("Looks good: " + r.harnesses + " harnesses, " + r.runners + " runners, " + r.presets + " presets, " + r.nodes + " nodes, " + r.workflows + " workflows."); })
          .catch(function (e) { show(e.message, true); });
      } }, "Check");
      var saveBtn = h("button", { class: "primary", type: "button", disabled: !c.editable, onclick: function () {
        saveBtn.disabled = true;
        save({ text: ta.value }, "Saved the file").catch(function (e) { show(e.message, true); saveBtn.disabled = false; });
      } }, "Save");
      if (!c.editable) ta.readOnly = true;
      body.appendChild(h("p", { class: "muted", text: "The whole file. Comments and layout are yours; the forms keep them too, except inside the section they rewrite. `indra init` writes a starter with every option explained." }));
      body.appendChild(ta); body.appendChild(out);
      body.appendChild(h("div", { class: "row" }, check, saveBtn,
        h("button", { type: "button", onclick: function () { saveFile("indra.toml", ta.value, "application/toml"); } }, "Download indra.toml"),
        h("button", { type: "button", disabled: !c.editable, onclick: function () {
          pickFiles({ accept: ".toml,text/plain" }).then(function (fs) {
            if (!fs.length) return;
            ta.value = fs[0].text; show("Loaded " + fs[0].name + " into the editor. Check it, then Save to apply it.");
          });
        } }, "Load a file")));
    }

    load().catch(function (e) { body.textContent = ""; body.appendChild(empty(e.status === 404 ? "This console was started without access to a config file." : e.message)); });
  }

  // ---- router --------------------------------------------------------------------
  function setHarness(name) {
    S.h = name; store("indra.h", name);
    $("harness").value = name;
    route();
  }

  function refreshOverview() {
    return api("/api/overview").then(function (ov) {
      S.ov = ov;
      if (!ov.harnesses.some(function (x) { return x.name === S.h; })) S.h = ov.harnesses.length ? ov.harnesses[0].name : "";
      var sel = $("harness"); sel.textContent = "";
      ov.harnesses.forEach(function (x) { sel.appendChild(h("option", { value: x.name, text: x.name })); });
      sel.value = S.h;
      var ver = $("version"); ver.textContent = "v" + ov.version; ver.title = "";
      var up = ov.update || {};
      if (up.available) {
        ver.appendChild(h("span", { class: "pill warn small", text: up.auto ? "v" + up.available + " installed: restart" : "v" + up.available + " out" }));
        ver.title = up.auto
          ? (up.error ? "Could not update itself: " + up.error + ". Run indra update." : "indra " + up.available + " was installed by the automatic update; restart indra serve to use it.")
          : "indra " + up.available + " is available: run indra update (or indra update --auto on to stay up to date by itself).";
      }
      $("ro-badge").hidden = !ov.read_only;
      // Who is signed in, and what only admins can open.
      var who = $("who"); who.textContent = "";
      if (ov.users) {
        who.appendChild(h("span", { class: "muted small", text: ov.user + " (" + ov.role + ")" }));
        if (ov.user !== "token") who.appendChild(h("button", { type: "button", class: "ghost small", onclick: changeOwnPassword }, "Password"));
        who.appendChild(h("button", { type: "button", class: "ghost small", onclick: signOut }, ov.user === "token" ? "Forget" : "Sign out"));
      }
      Array.prototype.forEach.call(document.querySelectorAll('#nav a[data-route="settings"], #nav a[data-route="terminal"]'), function (a) { a.hidden = !ov.is_admin; });
      var b = $("running-badge"); b.hidden = !ov.running; b.textContent = String(ov.running);
      var a = $("approvals-badge"); a.hidden = !ov.approvals; a.textContent = String(ov.approvals || 0);
      // Agents waiting for an answer show in the tab's title too.
      document.title = (ov.approvals ? "(" + ov.approvals + ") " : "") + "indra";
      a.title = ov.approvals ? ov.approvals + " agent request(s) wait for your answer" : "";
      a.title = ov.approvals ? ov.approvals + " tool request(s) waiting for you" : "";
      var l = $("learnings-badge"); l.hidden = !ov.learnings; l.textContent = String(ov.learnings || 0);
      l.title = ov.learnings ? ov.learnings + " learning(s) waiting for review" : "";
    });
  }

  function route() {
    S.cleanup.forEach(function (f) { try { f(); } catch (e) { /* ignore */ } });
    S.cleanup = [];
    var parts = (location.hash.replace(/^#\/?/, "") || "overview").split("/");
    var page = parts[0], arg = parts.length > 1 ? decodeURIComponent(parts.slice(1).join("/")) : undefined;
    var navKey = { def: "agents", run: "runs", proc: "sessions", procs: "sessions", workflow: "workflows", wfedit: "workflows", memory: "knowledge", delegate: "launch", plan: "sessions" }[page] || page;
    Array.prototype.forEach.call(document.querySelectorAll("#nav a"), function (a) {
      var on = a.dataset.route === navKey; a.classList.toggle("on", on);
      if (on) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
    });
    // The page is under More: show it open.
    var more = $("nav-more"), inMore = more && more.querySelector('a[data-route="' + navKey + '"]');
    if (inMore) more.open = true;
    var m = $("main"); m.textContent = ""; m.scrollTop = 0;
    // A newer navigation wins: a slower, earlier one must not render too.
    var gen = S.gen = (S.gen || 0) + 1;
    refreshOverview().then(function () {
      if (gen !== S.gen) return;
      if (page === "def" && arg) {
        var kind = arg.split(":")[0];
        Array.prototype.forEach.call(document.querySelectorAll("#nav a"), function (a) { a.classList.toggle("on", a.dataset.route === KIND_ROUTE[kind]); });
      }
      var pages = {
        overview: function () { pageOverview(m); },
        agents: function () { pageDefs(m, "agent"); },
        skills: function () { pageDefs(m, "skill"); },
        prompts: function () { pageDefs(m, "prompt"); },
        def: function () { pageDef(m, arg); },
        graph: function () { pageGraph(m, arg); },
        memory: function () { pageKnowledge(m, "graph/" + (arg || "")); },
        knowledge: function () { pageKnowledge(m, arg); },
        runs: function () { pageRuns(m); },
        run: function () { pageRun(m, arg); },
        nodes: function () { pageNodes(m); },
        workflows: function () { pageWorkflows(m); },
        workflow: function () { pageWorkflowRun(m, arg); },
        launch: function () { pageLaunch(m); },
        procs: function () { pageProcs(m); },
        sessions: function () { pageSessions(m, arg); },
        proc: function () { pageProc(m, arg); },
        settings: function () { pageSettings(m, arg); },
        board: function () { pageBoard(m); },
        accounts: function () { pageAccounts(m); },
        wfedit: function () { pageWorkflowEditor(m, arg); },
        terminal: function () { pageTerminal(m); },
        delegate: function () { pageDelegate(m); },
        market: function () { pageMarket(m, arg); },
        tools: function () { pageTools(m); },
        plan: function () { pagePlan(m, arg); }
      };
      (pages[page] || pages.overview)();
    }).catch(function (e) { if (e.message !== "unauthorized") { m.appendChild(empty(e.message)); } });
  }

  $("harness").addEventListener("change", function (e) { setHarness(e.target.value); });
  $("theme").addEventListener("click", function () {
    var dark = cssVar("--bg") === "#131312";
    var t = dark ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", t); store("indra.theme", t);
    // Only what draws with theme colors redraws: what you typed stays.
    window.dispatchEvent(new Event("indra-theme"));
  });
  var savedTheme = store("indra.theme");
  if (savedTheme === "light" || savedTheme === "dark") document.documentElement.setAttribute("data-theme", savedTheme);
  // Unsaved changes: typing in a form marks the page; leaving it asks first.
  // A successful save (any API call that changes something) clears the mark.
  S.dirty = false; S.hash = location.hash;
  $("main").addEventListener("input", function (e) {
    var t = e.target;
    if (!t || t.type === "search" || t.closest("[data-nodirty]")) return;
    if (t.closest("form, .form, .editor, .wfeditor")) S.dirty = true;
  });
  window.addEventListener("beforeunload", function (e) { if (S.dirty) { e.preventDefault(); e.returnValue = ""; } });
  window.addEventListener("hashchange", function () {
    if (S.dirty && location.hash !== S.hash) {
      if (!window.confirm("You have changes that are not saved. Leave this page anyway?")) {
        history.replaceState(null, "", S.hash || "#/");
        return;
      }
    }
    S.dirty = false; S.hash = location.hash;
    route();
  });
  route();
})();
