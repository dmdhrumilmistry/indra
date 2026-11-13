// indra UI. Plain JavaScript, no build step, no third-party code.
// Every string from the server is rendered with textContent, never as HTML.
(function () {
  "use strict";

  // ---- tiny helpers ----------------------------------------------------------
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
        else if (k === "value") el.value = v;
        else if (k === "hidden") el.hidden = !!v;
        else el.setAttribute(k, v === true ? "" : v);
      });
    }
    kids.forEach(function add(c) {
      if (c === null || c === undefined || c === false) return;
      if (Array.isArray(c)) c.forEach(add);
      else el.appendChild(typeof c === "string" || typeof c === "number" ? document.createTextNode(String(c)) : c);
    });
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
  function bytes(n) { return n < 1024 ? n + " B" : n < 1048576 ? (n / 1024).toFixed(1) + " KB" : (n / 1048576).toFixed(1) + " MB"; }
  function store(k, v) { try { if (v === undefined) return localStorage.getItem(k); localStorage.setItem(k, v); } catch (e) { return null; } }
  function toast(msg, bad) {
    var t = h("div", { class: "toast" + (bad ? " bad" : ""), role: "status", text: msg });
    document.body.appendChild(t);
    setTimeout(function () { t.remove(); }, bad ? 6000 : 2600);
  }

  var S = { ov: null, h: store("indra.h") || "", cleanup: [] };

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
        return data;
      });
    });
  }
  function hq(extra) { var p = { h: S.h }; Object.keys(extra || {}).forEach(function (k) { p[k] = extra[k]; }); return q(p); }
  function harness() { return S.ov && (S.ov.harnesses.find(function (x) { return x.name === S.h; }) || S.ov.harnesses[0]); }

  function authWall() {
    var m = $("main");
    m.textContent = "";
    m.appendChild(h("div", { class: "authwall card" },
      h("h1", { text: "Access token needed" }),
      h("p", { text: "indra printed a URL containing an access token when it started. Open that URL once in this browser; it sets a cookie and you will not need it again until indra restarts." }),
      h("p", { class: "muted", text: "The token protects the console because it can start agents and edit prompts on this machine." })));
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
    S.cleanup.push(function () { mq.removeEventListener("change", redraw); });
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
    m.appendChild(header("Overview", "Harnesses indra found, what they contain, and what is running."));
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
    m.appendChild(h("h2", { text: "Runners" }));
    m.appendChild(h("div", { class: "chips" }, ov.runners.map(function (r) {
      return h("span", { class: "pill " + (r.available ? "ok" : ""), title: r.program || (r.command + " is not on PATH"), text: r.name + (r.available ? "" : " (not installed)") });
    })));
    var running = ov.procs.filter(function (p) { return p.status === "running"; });
    m.appendChild(h("h2", { text: "Running agents" }));
    m.appendChild(running.length ? procTable(running) : empty("Nothing running. Start one from Launch."));
  }

  function pageDefs(m, kind) {
    var titles = { agent: "Agents", skill: "Skills", prompt: "Prompts" };
    var subs = { agent: "Agent definitions: who does what, on which model, with which tools.",
      skill: "Skills the agents load. Click one to read or edit it.", prompt: "Reference documents and prompt files the harness uses." };
    m.appendChild(header(titles[kind], subs[kind]));
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
        h("span", { class: "pill", text: d.kind }), d.model ? h("span", { class: "pill", text: d.model }) : null]));
      var ta = h("textarea", { class: "code", spellcheck: "false", "aria-label": "File content", value: original });
      var save = h("button", { class: "primary", type: "button", disabled: true }, "Save");
      var revert = h("button", { type: "button", disabled: true }, "Revert");
      var state = h("span", { class: "muted", text: S.ov.read_only ? "Read-only: editing is disabled." : "" });
      if (S.ov.read_only) ta.readOnly = true;
      function dirty() { var x = ta.value !== original; save.disabled = !x || S.ov.read_only; revert.disabled = !x; state.textContent = x ? "Unsaved changes" : ""; }
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

  function pageMemory(m, idx) {
    m.appendChild(header("Memory", "Memory vaults this harness reads and writes: past findings, learnings and their relations."));
    api("/api/memory" + hq()).then(function (list) {
      if (!list.length) { m.appendChild(empty("This harness has no memory vaults configured.")); return; }
      var cur = idx === undefined ? (list.find(function (v) { return v.exists && v.notes; }) || list[0]).index : Number(idx);
      m.appendChild(h("div", { class: "grid" }, list.map(function (v) {
        return h("div", { class: "card click" + (v.index === cur ? " sel" : ""), tabindex: "0", role: "button",
          onclick: function () { location.hash = "#/memory/" + v.index; } },
          h("div", { class: "cmd", text: v.path }),
          h("div", { class: "stats" }, h("div", { class: "stat" }, h("b", { text: String(v.notes) }), h("span", { text: "notes" })),
            h("div", { class: "stat" }, h("b", { text: String(v.links) }), h("span", { text: "links" }))),
          v.exists ? null : h("p", { class: "muted", text: "Not created yet: the first review run with memory enabled creates it." }));
      })));
      var v = list.find(function (x) { return x.index === cur; });
      if (!v || !v.exists) return;
      m.appendChild(h("h2", { text: "Graph" }));
      api("/api/memory/graph" + hq({ i: cur })).then(function (g) {
        var gv = graphView(m, g, function (n, panel) {
          panel.appendChild(h("h3", { text: n.type }));
          panel.appendChild(h("h2", { text: n.title }));
          if (n.raw.tags && n.raw.tags.length) panel.appendChild(h("div", { class: "chips" }, n.raw.tags.map(function (t) { return h("span", { class: "pill", text: "#" + t }); })));
          if (n.raw.body) panel.appendChild(h("pre", { text: n.raw.body.trim() }));
          panel.appendChild(h("div", null, linkList("Links out", n.out.map(function (l) { return { rel: l.rel, title: l.n.title, id: l.n.id }; }), function (l) { gv.select(l.id); })));
          panel.appendChild(h("div", null, linkList("Backlinks", n.inc.map(function (l) { return { rel: l.rel, title: l.n.title, id: l.n.id }; }), function (l) { gv.select(l.id); })));
        });
      });
    });
  }

  function segs(stages) { return h("div", { class: "segs", title: stages.map(function (s) { return s.name + ": " + s.state; }).join(", ") }, stages.map(function (s) { return h("i", { class: s.state }); })); }

  function pageRuns(m) {
    var hs = harness();
    m.appendChild(header("Runs", hs && hs.has_runs ? "Run directories under " + hs.targets.join(", ") + ". Refreshes every 3 seconds." : "This harness does not define a runs directory."));
    var holder = h("div", { class: "tablewrap" });
    m.appendChild(holder);
    function load() {
      api("/api/runs" + hq()).then(function (runs) {
        holder.textContent = "";
        if (!runs.length) { holder.appendChild(empty("No runs yet. Start one from Launch, or run the harness from your agent.")); return; }
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

  function pageRun(m, key) {
    var top = h("div"), body = h("div"), viewer = h("div");
    m.appendChild(top); m.appendChild(body);
    var openRel = null, lastMod = 0;
    function showFile(rel) {
      openRel = rel;
      Array.prototype.forEach.call(body.querySelectorAll(".files a"), function (a) { a.classList.toggle("on", a.dataset.rel === rel); });
      api("/api/run/file" + hq({ key: key, rel: rel })).then(function (f) {
        viewer.textContent = "";
        viewer.appendChild(h("div", { class: "row" }, h("b", { text: f.rel }), f.truncated ? h("span", { class: "pill warn", text: "showing the end of a large file" }) : null, h("span", { class: "muted", text: ago(f.modified) })));
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

  function pageLaunch(m) {
    var ov = S.ov, hs = harness();
    m.appendChild(header("Launch an agent", "Runs a configured runner in a target directory with your prompt, and streams what it does."));
    if (ov.read_only) { m.appendChild(h("div", { class: "note warn", text: "indra is running read-only, so launching is disabled." })); return; }
    if (!hs) { m.appendChild(empty("No harness configured.")); return; }
    var avail = ov.runners.filter(function (r) { return r.available; });
    if (!avail.length) { m.appendChild(h("div", { class: "note warn", text: "No runner is installed. Install Claude Code, Codex or Gemini CLI, or add a [[runner]] to indra.toml." })); return; }
    var runner = h("select", { "aria-label": "Runner" }, avail.map(function (r) { return h("option", { value: r.name, text: r.name + "  (" + r.command + ")" }); }));
    var target = h("select", { "aria-label": "Target" }, hs.targets.map(function (t, i) { return h("option", { value: String(i), text: t }); }));
    var presets = ov.presets.filter(function (p) { return !p.harness || p.harness === hs.name; });
    var preset = h("select", { "aria-label": "Preset" }, h("option", { value: "", text: "Write your own" }), presets.map(function (p, i) { return h("option", { value: String(i), text: p.name }); }));
    var prompt = h("textarea", { placeholder: "What should the agent do?", "aria-label": "Prompt" });
    var preview = h("div", { class: "cmd" });
    var go = h("button", { class: "primary", type: "submit" }, "Launch");
    function update() {
      var r = ov.runners.find(function (x) { return x.name === runner.value; });
      var t = hs.targets[Number(target.value)] || "";
      var p = prompt.value.split("{target}").join(t);
      preview.textContent = r ? [r.command].concat(r.args.map(function (a) { return a.split("{target}").join(t).split("{prompt}").join(p); }))
        .map(function (a) { return /[\s"'$`\\;&|<>]/.test(a) ? JSON.stringify(a) : a; }).join(" ") : "";
      go.disabled = !prompt.value.trim();
    }
    preset.addEventListener("change", function () {
      var p = presets[Number(preset.value)];
      if (p) { prompt.value = p.prompt; if (p.runner && avail.some(function (r) { return r.name === p.runner; })) runner.value = p.runner; }
      update();
    });
    [runner, target, prompt].forEach(function (el) { el.addEventListener("input", update); el.addEventListener("change", update); });
    var form = h("form", { class: "form", onsubmit: function (e) {
      e.preventDefault(); go.disabled = true;
      api("/api/procs", { method: "POST", body: { h: hs.name, runner: runner.value, prompt: prompt.value, target: Number(target.value) } })
        .then(function (r) { location.hash = "#/proc/" + r.id; })
        .catch(function (err) { toast(err.message, true); go.disabled = false; });
    } },
      h("label", null, "Preset", preset),
      h("label", null, "Prompt", h("small", { text: "{target} becomes the target path." }), prompt),
      h("div", { class: "row" }, h("label", null, "Runner", runner), h("label", null, "Target", target)),
      h("label", null, "Command", h("small", { text: "Executed directly, never through a shell. The prompt is one argument." }), preview),
      h("div", { class: "note", text: "The agent runs with your user's permissions and your agent CLI's own permission settings (for Claude Code: .claude/settings.json). Tool calls it is not allowed to make are refused, and show up in the feed." }),
      h("div", { class: "row" }, go));
    m.appendChild(form);
    update();
  }

  function statusPill(s) {
    var cls = { running: "run live", exited: "ok", failed: "bad", stopped: "warn" }[s] || "";
    return h("span", { class: "pill " + cls, text: s });
  }
  function procTable(list) {
    return h("div", { class: "tablewrap" }, h("table", null,
      h("thead", null, h("tr", null, ["#", "Status", "Runner", "Prompt", "Agents", "Cost", "Started"].map(function (t) { return h("th", { text: t }); }))),
      h("tbody", null, list.map(function (p) {
        var go = function () { location.hash = "#/proc/" + p.id; };
        return h("tr", { class: "click", tabindex: "0", onclick: go, onkeydown: function (e) { if (e.key === "Enter") go(); } },
          h("td", { text: String(p.id) }), h("td", null, statusPill(p.status)), h("td", { text: p.runner }),
          h("td", { class: "desc" }, h("div", { text: p.prompt.length > 90 ? p.prompt.slice(0, 88) + "..." : p.prompt }), p.last ? h("div", { class: "muted", text: p.last }) : null),
          h("td", { text: String(p.agents.length) }),
          h("td", { text: p.cost_usd != null ? "$" + p.cost_usd.toFixed(4) : "-" }),
          h("td", { class: "muted", text: ago(p.started) }));
      }))));
  }

  function pageProcs(m) {
    m.appendChild(header("Processes", "Agents started from this console since it started. Logs stay on disk under " + S.ov.data_dir + "."));
    var holder = h("div"); m.appendChild(holder);
    function load() {
      api("/api/procs").then(function (list) {
        holder.textContent = "";
        holder.appendChild(list.length ? procTable(list) : empty("Nothing launched yet."));
      });
    }
    load(); every(2000, load);
  }

  function pageProc(m, id) {
    var top = h("div"), agentsBox = h("div"), toolsBox = h("div"), feed = h("div", { class: "feed", role: "log", "aria-live": "polite" });
    var follow = h("input", { type: "checkbox", checked: true, id: "follow" });
    m.appendChild(top);
    m.appendChild(h("div", { class: "split" },
      h("div", null, h("div", { class: "row" }, h("h2", { text: "Live feed" }), h("label", { class: "row muted" }, follow, "Follow")), feed),
      h("div", null, h("div", { class: "card" }, h("h3", { text: "Subagents" }), agentsBox, h("h3", { text: "Tool calls" }), toolsBox))));
    function renderInfo(p) {
      top.textContent = "";
      var stop = h("button", { class: "danger", type: "button", disabled: p.status !== "running" || S.ov.read_only, onclick: function () {
        stop.disabled = true;
        api("/api/procs/" + id + "/stop", { method: "POST" }).then(function () { toast("Stopping"); }).catch(function (e) { toast(e.message, true); });
      } }, "Stop");
      top.appendChild(header("Process #" + p.id + " - " + p.runner, p.cwd, [statusPill(p.status), p.cost_usd != null ? h("span", { class: "pill", text: "$" + p.cost_usd.toFixed(4) }) : null, stop]));
      top.appendChild(h("div", { class: "cmd", text: p.prompt }));
      agentsBox.textContent = "";
      agentsBox.appendChild(p.agents.length ? h("div", { class: "bars" }, p.agents.map(function (a) {
        return h("div", null, h("div", { class: "row" }, h("b", { text: a.type }), statusPill(a.state === "running" ? "running" : a.state === "error" ? "failed" : "exited"),
          h("span", { class: "muted", text: a.tools + " tools" })), h("div", { class: "muted", text: a.description }));
      })) : h("p", { class: "muted", text: p.runner === "claude" ? "None spawned yet." : "This runner does not report subagents." }));
      toolsBox.textContent = "";
      toolsBox.appendChild(bars(p.tools));
    }
    function refresh() { api("/api/procs/" + id).then(renderInfo).catch(function (e) { top.textContent = ""; top.appendChild(empty(e.message)); }); }
    refresh();
    var es = new EventSource("/api/procs/" + id + "/events");
    var pending = null;
    es.onmessage = function (msg) {
      var e; try { e = JSON.parse(msg.data); } catch (x) { return; }
      feed.appendChild(h("div", { class: "ev " + e.kind }, h("time", { text: clock(e.ts) }), h("span", { class: "k", text: e.kind }),
        h("div", { class: "t" }, e.agent ? h("span", { class: "a", text: e.agent }) : null, e.text)));
      while (feed.childNodes.length > 3000) feed.removeChild(feed.firstChild);
      if (follow.checked) feed.scrollTop = feed.scrollHeight;
      if (e.kind === "agent" || e.kind === "tool" || e.kind === "exit" || e.kind === "result") {
        clearTimeout(pending); pending = setTimeout(refresh, 300);
      }
    };
    S.cleanup.push(function () { es.close(); clearTimeout(pending); });
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
      $("version").textContent = "v" + ov.version;
      $("ro-badge").hidden = !ov.read_only;
      var b = $("running-badge"); b.hidden = !ov.running; b.textContent = String(ov.running);
    });
  }

  function route() {
    S.cleanup.forEach(function (f) { try { f(); } catch (e) { /* ignore */ } });
    S.cleanup = [];
    var parts = (location.hash.replace(/^#\/?/, "") || "overview").split("/");
    var page = parts[0], arg = parts.length > 1 ? decodeURIComponent(parts.slice(1).join("/")) : undefined;
    var navKey = { def: "agents", run: "runs", proc: "procs" }[page] || page;
    Array.prototype.forEach.call(document.querySelectorAll("#nav a"), function (a) {
      var on = a.dataset.route === navKey; a.classList.toggle("on", on);
      if (on) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
    });
    var m = $("main"); m.textContent = ""; m.scrollTop = 0;
    refreshOverview().then(function () {
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
        memory: function () { pageMemory(m, arg); },
        runs: function () { pageRuns(m); },
        run: function () { pageRun(m, arg); },
        launch: function () { pageLaunch(m); },
        procs: function () { pageProcs(m); },
        proc: function () { pageProc(m, arg); }
      };
      (pages[page] || pages.overview)();
    }).catch(function (e) { if (e.message !== "unauthorized") { m.appendChild(empty(e.message)); } });
  }

  $("harness").addEventListener("change", function (e) { setHarness(e.target.value); });
  $("theme").addEventListener("click", function () {
    var dark = cssVar("--bg") === "#131312";
    var t = dark ? "light" : "dark";
    document.documentElement.setAttribute("data-theme", t); store("indra.theme", t);
    route();
  });
  var savedTheme = store("indra.theme");
  if (savedTheme === "light" || savedTheme === "dark") document.documentElement.setAttribute("data-theme", savedTheme);
  window.addEventListener("hashchange", route);
  route();
})();
