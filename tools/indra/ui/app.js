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
    Array.prototype.forEach.call(document.querySelectorAll(".toast"), function (x) { x.remove(); });
    var t = h("div", { class: "toast" + (bad ? " bad" : ""), role: "status", text: msg });
    document.body.appendChild(t);
    setTimeout(function () { t.remove(); }, bad ? 6000 : 2600);
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
    var ro = S.ov.read_only;
    m.appendChild(header(titles[kind], subs[kind], ro ? null : [
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
        S.ov.read_only ? null : h("button", { type: "button", class: "danger", onclick: function () {
          if (!window.confirm("Delete " + d.path + "? This removes the file.")) return;
          api("/api/def" + hq({ id: d.id }), { method: "DELETE" }).then(function () {
            toast("Deleted " + d.path); location.hash = "#/" + ({ agent: "agents", skill: "skills", prompt: "prompts" }[d.kind] || "overview");
          }).catch(function (e) { toast(e.message, true); });
        } }, "Delete")]));
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
        top.appendChild(h("div", { class: "grid" }, vaults.map(function (x) {
          return h("div", { class: "card click" + (x.index === cur ? " sel" : ""), tabindex: "0", role: "button",
            onclick: function () { go(tab === "learnings" ? "notes" : tab, x.index); } },
            h("div", { class: "cmd", text: x.path }),
            x.exists ? h("div", { class: "stats" },
              h("div", { class: "stat" }, h("b", { text: String(x.notes) }), h("span", { text: "notes" })),
              h("div", { class: "stat" }, h("b", { text: String(x.links) }), h("span", { text: "links" })),
              h("div", { class: "stat" }, h("b", { text: String(x.dangling) }), h("span", { text: "dangling" })))
              : h("div", null, h("p", { class: "muted", text: "Not created yet." }), ro ? null : h("button", { type: "button", onclick: function (e) {
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
      body.appendChild(h("p", { class: "muted", text: "Agents launched with \"Ask for learnings\" (or a profile with reflect = true) propose what they learned before they finish. Nothing reaches memory until you accept it: it then becomes a learning note, with where it came from, and later agents are given it when it is relevant." }));
      body.appendChild(holder);
      var existing = vaults.filter(function (v) { return true; });
      function vaultSelect() {
        var s = h("select", { "aria-label": "Vault" }, existing.map(function (v) { return h("option", { value: String(v.index), text: v.path + (v.exists ? "" : " (created on accept)") }); }));
        s.value = String(cur);
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
          if (done.length) holder.appendChild(h("details", { class: "card" }, h("summary", { text: "Accepted (" + done.length + ")" }),
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
          ro ? null : h("div", { class: "row" }, h("label", null, "Into", vs), h("label", null, "Links", links)),
          ro ? null : h("div", { class: "row" },
            h("button", { type: "button", class: "primary", onclick: function () {
              patch().then(function () { return api("/api/learnings/" + l.id + "/accept", { method: "POST", body: { h: S.h, i: Number(vs.value), links: csv(links.value) } }); })
                .then(function (r) { toast((r.note.created ? "Saved as " : "Merged into ") + r.note.id); load(); refreshOverview(); })
                .catch(function (err) { toast(err.message, true); });
            } }, "Accept into memory"),
            h("button", { type: "button", onclick: function () { patch().then(function () { toast("Saved"); }).catch(function (err) { toast(err.message, true); }); } }, "Save edits"),
            h("button", { type: "button", class: "danger", onclick: function () {
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

  // Nodes, fetched once per page that needs them.
  function loadNodes(refresh) { return api("/api/nodes" + hq(refresh ? { refresh: 1 } : {})); }
  function runnersOn(node, ov) {
    if (node.name === "local") return ov.runners.filter(function (r) { return r.available; }).map(function (r) { return r.name; });
    return node.info ? node.info.runners.filter(function (r) { return r.available; }).map(function (r) { return r.name; }) : [];
  }

  function pageLaunch(m) {
    var ov = S.ov, hs = harness();
    m.appendChild(header("Launch an agent", "Runs a runner on any node, in one of its target directories, and streams what it does."));
    if (ov.read_only) { m.appendChild(h("div", { class: "note warn", text: "indra is running read-only, so launching is disabled." })); return; }
    if (!hs) { m.appendChild(empty("No harness configured.")); return; }
    var holder = h("div", { class: "muted", text: "Checking nodes..." });
    m.appendChild(holder);
    loadNodes(false).then(function (nodes) {
      holder.remove();
      var node = h("select", { "aria-label": "Node" }, nodes.map(function (n) {
        return h("option", { value: n.name, text: n.name + (n.kind === "local" ? "  (this machine)" : "  (" + n.ssh + (n.error ? ", unreachable" : "") + ")") });
      }));
      var runner = h("select", { "aria-label": "Runner" });
      var target = h("select", { "aria-label": "Target" });
      var presets = ov.presets.filter(function (p) { return !p.harness || p.harness === hs.name; });
      var preset = h("select", { "aria-label": "Preset" }, h("option", { value: "", text: "Write your own" }), presets.map(function (p, i) { return h("option", { value: String(i), text: p.name }); }));
      var prompt = h("textarea", { placeholder: "What should the agent do?", "aria-label": "Prompt" });
      var profiles = ov.profiles || [];
      var profile = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: "None (runner defaults)" }),
        profiles.map(function (p) { return h("option", { value: p.name, text: p.name + (p.description ? "  -  " + p.description : "") }); }));
      var model = h("input", { type: "text", list: "models", "aria-label": "Model", placeholder: "default" });
      var mode = h("select", { "aria-label": "Permission mode" }, h("option", { value: "", text: "default" }),
        MODES.map(function (x) { return h("option", { value: x[0], text: x[1] }); }));
      var effort = h("select", { "aria-label": "Effort" }, h("option", { value: "", text: "default" }),
        ["low", "medium", "high", "xhigh", "max"].map(function (x) { return h("option", { value: x, text: x }); }));
      var memory = h("input", { type: "checkbox" }), reflect = h("input", { type: "checkbox" });
      var memNote = h("div", { class: "muted small" });
      function prof() { return profiles.find(function (p) { return p.name === profile.value; }); }
      function fillProfile() {
        var p = prof() || {};
        memory.checked = !!p.memory; reflect.checked = !!p.reflect;
        if (p.runner && Array.prototype.some.call(runner.options, function (o) { return o.value === p.runner; })) runner.value = p.runner;
        model.placeholder = p.model || "default"; mode.options[0].text = p.permission_mode ? "profile: " + p.permission_mode : "default";
        effort.options[0].text = p.effort ? "profile: " + p.effort : "default";
        update();
      }
      profile.addEventListener("change", fillProfile);
      [model, mode, effort].forEach(function (el) { el.addEventListener("input", update); el.addEventListener("change", update); });
      var preview = h("div", { class: "cmd" });
      var go = h("button", { class: "primary", type: "submit" }, "Launch");
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
          }
          var viaStdin = r && r.format === "claude-stream-json" && r.args.indexOf("{prompt}") >= 0;
          preview.textContent = r ? [r.command].concat(r.args.filter(function (a) { return !(viaStdin && a === "{prompt}"); })
            .map(function (a) { return a.split("{target}").join(t).split("{prompt}").join(p); })).concat(viaStdin ? ["--input-format", "stream-json"] : []).concat(extra)
            .map(function (a) { return /[\s"'$`\;&|<>]/.test(a) ? JSON.stringify(a) : a; }).join(" ") : "";
          if (viaStdin) preview.textContent += "\n(the prompt goes over stdin, so you can approve its tool requests and change its mode and model while it runs)";
        } else {
          preview.textContent = "ssh " + n.ssh + " -- indra node exec   (job: " + JSON.stringify({ runner: runner.value, target: t, prompt: p }) + ")";
        }
        var why = !runner.value ? (n.error ? "This node is unreachable: " + n.error : "No runner is installed on this node.") : !n.targets.length ? "This node has no targets configured." : "";
        note.textContent = why; note.hidden = !why;
        go.disabled = !prompt.value.trim() || !runner.value || !n.targets.length;
      }
      var note = h("div", { class: "note warn", hidden: true });
      preset.addEventListener("change", function () {
        var p = presets[Number(preset.value)];
        if (p) { prompt.value = p.prompt; if (p.runner) { runner.value = p.runner; } if (p.profile) { profile.value = p.profile; fillProfile(); } }
        update();
      });
      node.addEventListener("change", fillNode);
      [runner, target, prompt].forEach(function (el) { el.addEventListener("input", update); el.addEventListener("change", update); });
      var form = h("form", { class: "form", onsubmit: function (e) {
        e.preventDefault(); go.disabled = true;
        var o = {};
        if (model.value.trim()) o.model = model.value.trim();
        if (mode.value) o.permission_mode = mode.value;
        if (effort.value) o.effort = effort.value;
        api("/api/procs", { method: "POST", body: { h: hs.name, node: node.value, runner: runner.value, prompt: prompt.value, target: Number(target.value), profile: profile.value || null, opts: o, memory: memory.checked, reflect: reflect.checked } })
          .then(function (r) { location.hash = "#/proc/" + r.id; })
          .catch(function (err) { toast(err.message, true); go.disabled = false; });
      } },
        h("label", null, "Preset", preset),
        h("label", null, "Prompt", h("small", { text: "{target} becomes the target path, {node} the node name." }), prompt),
        h("div", { class: "row" }, h("label", null, "Node", node), h("label", null, "Runner", runner), h("label", null, "Target", target)),
        h("div", { class: "row" }, h("label", null, "Profile", profile), h("label", null, "Model", model), h("label", null, "Permission mode", mode), h("label", null, "Effort", effort)),
        modelList(),
        h("div", { class: "card knowctl" },
          h("label", { class: "check" }, memory, h("span", null, h("b", { text: "Give it relevant memory" }), h("small", { text: " - the notes from this harness's vaults that best match the prompt are added after it" }))),
          h("div", { class: "row" }, h("button", { type: "button", class: "ghost", onclick: function () {
            var p = prompt.value.trim(); if (!p) { memNote.textContent = "Write the prompt first."; return; }
            memNote.textContent = "Recalling...";
            api("/api/knowledge/recall" + hq({ q: p })).then(function (r) {
              memNote.textContent = r.notes.length ? "It would get: " + r.notes.map(function (n) { return n.title; }).join("; ") : "Nothing relevant in memory for this prompt.";
            }).catch(function (e) { memNote.textContent = e.message; });
          } }, "Preview what it would get"), memNote),
          h("label", { class: "check" }, reflect, h("span", null, h("b", { text: "Ask for learnings at the end" }), h("small", { text: " - before it finishes, a Claude Code agent proposes what it learned; review them under Knowledge > Learnings" })))),
        note,
        h("label", null, "Command", h("small", { text: "Executed directly, never through a shell. On a remote node the job travels as JSON over SSH." }), preview),
        h("div", { class: "note", text: "The agent runs with that machine's user permissions and its agent CLI's own permission settings. A remote node runs only runners from its own configuration." }),
        h("div", { class: "row" }, go, canEdit() ? h("button", { type: "button", onclick: function () {
          var name = window.prompt("Save this prompt as a preset named:");
          if (!name || !name.trim()) return;
          var p = { name: name.trim(), prompt: prompt.value, harness: hs.name };
          if (runner.value) p.runner = runner.value;
          if (profile.value) p.profile = profile.value;
          addToSection("preset", p).then(function () { toast("Saved preset " + p.name); })
            .catch(function (err) { toast(err.message, true); });
        } }, "Save as preset") : null, h("a", { href: "#/workflows", text: "Or fan out to many targets at once" })));
      m.appendChild(form);
      fillNode();
    }).catch(function (e) { holder.textContent = e.message; });
  }

  // ---- nodes --------------------------------------------------------------------
  function meter(used, max) {
    var fill = h("div", { class: "fill" }); fill.style.width = Math.min(100, 100 * used / Math.max(1, max)) + "%";
    return h("div", { class: "bar" }, h("span", { text: "agents" }), h("div", { class: "track" }, fill), h("b", { text: used + "/" + max }));
  }
  function pageNodes(m) {
    var refresh = h("button", { type: "button" }, "Check again");
    m.appendChild(header("Nodes", "The machines agents run on. A machine dials in to this hub (Add a machine: no open port, no SSH, a direct WebRTC channel when possible), or is reached over SSH; either way it runs only its own configured runners.",
      [canEdit() ? h("button", { type: "button", class: "primary", onclick: function () { addMachine(); } }, "Add a machine") : null,
        canEdit() ? h("a", { class: "btn", href: "#/settings/nodes", text: "Edit nodes" }) : null, refresh]));
    var addBox = h("div"); m.appendChild(addBox);
    // A machine dials in: no open port, no SSH. One-time code, then its own key.
    function addMachine() {
      var name = h("input", { type: "text", required: true, placeholder: "build-box", "aria-label": "Name" });
      var targets = h("textarea", { class: "list", rows: "2", placeholder: "optional: codebases on it, one per line (it can also report its own with --target)" });
      var out = h("div");
      addBox.textContent = "";
      addBox.appendChild(h("form", { class: "form card", onsubmit: function (e) {
        e.preventDefault();
        var t = targets.value.split("\n").map(function (x) { return x.trim(); }).filter(Boolean);
        api("/api/nodes/enrol", { method: "POST", body: { name: name.value.trim(), targets: t } }).then(function (r) {
          out.textContent = "";
          out.appendChild(h("p", { text: "On that machine, with indra installed, run (the code works once, for 15 minutes):" }));
          out.appendChild(h("pre", { class: "view", text: r.command }));
          out.appendChild(h("p", { class: "muted small", text: "It dials out to this hub (no open port, no SSH), keeps the link up and reconnects on its own, and moves to a direct WebRTC channel when it can. Its key is saved on it; later starts need only `indra node connect <hub> --name " + name.value.trim() + "`. Behind a different address or a proxy, use the URL people use to reach this console." }));
          load(true);
        }).catch(function (err) { toast(err.message, true); });
      } }, h("h2", { text: "Add a machine" }), h("label", null, "Name", name), h("label", null, "Targets", targets),
        h("div", { class: "row" }, h("button", { class: "primary", type: "submit" }, "Get a connect command"), h("button", { type: "button", onclick: function () { addBox.textContent = ""; } }, "Close")), out));
      name.focus();
    }
    var grid = h("div", { class: "grid" }), runsBox = h("div");
    m.appendChild(grid); m.appendChild(runsBox);
    function showRuns(n) {
      runsBox.textContent = "";
      runsBox.appendChild(h("h2", { text: "Runs on " + n.name }));
      var box = h("div", { class: "muted", text: "Loading..." }); runsBox.appendChild(box);
      api("/api/nodes/runs" + hq({ node: n.name })).then(function (runs) {
        box.textContent = ""; box.className = "tablewrap";
        if (!runs.length) { box.appendChild(empty("No runs in this node's targets.")); return; }
        box.appendChild(h("table", null, h("thead", null, h("tr", null, ["Run", "Progress", "Results", "By agent", "Updated"].map(function (t) { return h("th", { text: t }); }))),
          h("tbody", null, runs.map(function (r) {
            var groups = Object.keys(r.counts.by_group).map(function (g) { return g + " " + r.counts.by_group[g]; }).join(", ");
            var go = n.name === "local" ? function () { location.hash = "#/run/" + encodeURIComponent(r.key); } : null;
            return h("tr", { class: go ? "click" : "", onclick: go },
              h("td", null, h("b", { text: r.name }), " ", r.active ? h("span", { class: "pill run live", text: "live" }) : null, h("div", { class: "muted", text: r.target })),
              h("td", null, segs(r.stages)), h("td", { text: String(r.counts.total) }), h("td", { class: "desc", text: groups || "-" }), h("td", { class: "muted", text: ago(r.modified) }));
          }))));
      }).catch(function (e) { box.textContent = e.message; });
    }
    function render(nodes) {
      grid.textContent = "";
      nodes.forEach(function (n) {
        var i = n.info, ok = !!i;
        var card = h("div", { class: "card" },
          h("div", { class: "row" }, h("h4", { text: n.name }), h("span", { class: "pill" + (n.transport === "webrtc" ? " ok" : ""),
            title: { webrtc: "Dialled in; a direct, encrypted WebRTC data channel", relay: "Dialled in; traffic relayed over its link to this hub", ssh: "Reached over SSH", none: "Not connected" }[n.transport] || "",
            text: n.kind === "local" ? "this machine" : n.transport === "webrtc" ? "link: direct (WebRTC)" : n.transport === "relay" ? "link: relayed" : n.transport === "none" ? "offline" : "ssh" }),
            h("span", { class: "pill " + (ok ? "ok" : "bad"), text: ok ? "online" : "unreachable" })),
          h("p", { class: "muted", text: n.kind === "local" ? (n.description || "") : n.ssh + (n.description ? "  -  " + n.description : "") }),
          ok ? h("div", { class: "stats" },
            [["host", i.hostname], ["os", i.os + " " + i.arch], ["cpus", String(i.cpus)], ["indra", i.version]].map(function (x) {
              return h("div", { class: "stat" }, h("b", { class: "small", text: x[1] }), h("span", { text: x[0] }));
            })) : h("div", { class: "note bad", text: n.error || "not checked yet" }),
          meter(n.running, n.max_parallel),
          ok ? [h("h3", { text: "Runners" }), h("div", { class: "chips" }, i.runners.map(function (r) { return h("span", { class: "pill " + (r.available ? "ok" : ""), text: r.name + (r.available ? "" : " (missing)") }); }))] : null,
          h("h3", { text: "Targets" }),
          n.targets.length ? h("div", { class: "files" }, n.targets.map(function (t) { return h("div", { class: "cmd", text: t }); })) : h("p", { class: "muted", text: "none configured" }),
          h("div", { class: "row" }, h("button", { type: "button", onclick: function () { showRuns(n); } }, "Show runs"),
            h("a", { href: "#/launch", text: "Launch here" })));
        grid.appendChild(card);
      });
      if (nodes.length === 1) grid.appendChild(h("div", { class: "card" }, h("h4", { text: "Add a remote machine" }),
        h("p", { class: "muted", text: "Install indra on it and make sure `ssh <host>` works without a password prompt, then add it here." }),
        canEdit() ? h("a", { class: "btn", href: "#/settings/nodes", text: "Add a node" })
          : h("pre", { class: "view", text: '[[node]]\nname = "build-box"\nssh = "me@build-box"\nmax_parallel = 2\ntargets = ["/srv/api"]' })));
    }
    function load(force) { grid.textContent = ""; grid.appendChild(h("div", { class: "muted", text: "Checking nodes..." })); loadNodes(force).then(render).catch(function (e) { grid.textContent = e.message; }); }
    refresh.addEventListener("click", function () { load(true); });
    load(false);
  }

  // ---- workflows ----------------------------------------------------------------
  var WF_STATE = { waiting: "", queued: "warn", running: "run live", succeeded: "ok", failed: "bad", skipped: "", cancelled: "warn" };
  function wfPill(s) { return h("span", { class: "pill " + (WF_STATE[s] || ""), text: s }); }

  function pageWorkflows(m) {
    var ov = S.ov, hs = harness();
    m.appendChild(header("Workflows", "Multi-step agent work across nodes: tasks run in dependency order, within each node's capacity, with retries. Run them here or with `indra run <name>`.",
      [canEdit() ? h("a", { class: "btn", href: "#/wfedit/", text: "New workflow" }) : null]));
    var runsBox = h("div"), defsBox = h("div", { class: "grid" });
    m.appendChild(h("h2", { text: "Runs" })); m.appendChild(runsBox);
    m.appendChild(h("h2", { text: "Configured workflows" })); m.appendChild(defsBox);
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
        defsBox.textContent = "";
        if (!w.defs.length) defsBox.appendChild(h("div", { class: "card" }, h("h4", { text: "No workflows configured" }),
          h("p", { class: "muted", text: "Chain agent tasks across machines, in dependency order. Or use a fan-out below." }),
          canEdit() ? h("a", { class: "btn", href: "#/wfedit/", text: "Create a workflow" }) : null));
        w.defs.forEach(function (d) {
          defsBox.appendChild(h("div", { class: "card" },
            h("div", { class: "row" }, h("h4", { text: d.name }), h("span", { class: "pill", text: d.task.length + " tasks" })),
            d.description ? h("p", { class: "muted", text: d.description }) : null,
            h("ul", { class: "links" }, d.task.map(function (t) {
              return h("li", null, h("b", { text: t.id }), h("span", { class: "muted", text: "  on " + t.node + ((t.needs || []).length ? ", after " + t.needs.join(", ") : "") }));
            })),
            h("div", { class: "row" }, h("button", { class: "primary", type: "button", disabled: ov.read_only, onclick: function (e) {
              e.target.disabled = true;
              api("/api/workflows", { method: "POST", body: { name: d.name } }).then(function (r) { location.hash = "#/workflow/" + r.id; })
                .catch(function (err) { toast(err.message, true); e.target.disabled = false; });
            } }, "Run"), canEdit() ? h("a", { href: "#/wfedit/" + encodeURIComponent(d.name), text: "Edit graph" }) : null)));
        });
      }).catch(function (e) { runsBox.textContent = e.message; });
    }
    loadRuns(); every(3000, loadRuns);

    if (ov.read_only || !hs) return;
    m.appendChild(h("h2", { text: "Fan out" }));
    var fan = h("div", { class: "card" }, h("p", { class: "muted", text: "Loading nodes..." }));
    m.appendChild(fan);
    loadNodes(false).then(function (nodes) {
      fan.textContent = "";
      var prompt = h("textarea", { "aria-label": "Prompt", placeholder: "/indra full security review of {target}" });
      prompt.value = "/indra full security review of {target}";
      var names = {}; nodes.forEach(function (n) { runnersOn(n, ov).forEach(function (r) { names[r] = 1; }); });
      var runner = h("select", { "aria-label": "Runner" }, Object.keys(names).map(function (r) { return h("option", { value: r, text: r }); }));
      var fprofile = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: "None" }), (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); }));
      var boxes = [];
      var picks = h("div", { class: "picks" }, nodes.map(function (n) {
        return h("fieldset", null, h("legend", null, n.name, " ", h("span", { class: "muted", text: n.error ? "(unreachable)" : "(" + n.running + "/" + n.max_parallel + " busy)" })),
          n.targets.length ? n.targets.map(function (t, i) {
            var cb = h("input", { type: "checkbox" }); boxes.push({ cb: cb, node: n.name, target: i });
            return h("label", { class: "pick" }, cb, h("span", { text: t }));
          }) : h("p", { class: "muted", text: "no targets" }));
      }));
      var go = h("button", { class: "primary", type: "submit" }, "Run on selected targets");
      fan.appendChild(h("form", { class: "form", onsubmit: function (e) {
        e.preventDefault();
        var targets = boxes.filter(function (b) { return b.cb.checked; }).map(function (b) { return { node: b.node, target: b.target }; });
        if (!targets.length) { toast("Pick at least one target", true); return; }
        go.disabled = true;
        api("/api/workflows/fanout", { method: "POST", body: { h: hs.name, runner: runner.value, prompt: prompt.value, targets: targets, profile: fprofile.value || null } })
          .then(function (r) { location.hash = "#/workflow/" + r.id; }).catch(function (err) { toast(err.message, true); go.disabled = false; });
      } },
        h("label", null, "Prompt", h("small", { text: "Runs once per selected target, in parallel within each node's capacity." }), prompt),
        h("div", { class: "row" }, h("label", null, "Runner", runner), h("label", null, "Profile", fprofile)), h("div", null, h("b", { text: "Targets" }), picks), h("div", { class: "row" }, go)));
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
          cancel.disabled = true; api("/api/workflows/" + id + "/cancel", { method: "POST" }).then(function () { toast("Cancelling"); }).catch(function (e) { toast(e.message, true); });
        } }, "Cancel");
        top.appendChild(header("Workflow #" + r.id + " - " + r.name, "Started " + ago(r.started) + (r.ended ? ", finished " + ago(r.ended) : ""), [wfPill(r.status), cancel]));
        var lv = depth(r.tasks), cols = [];
        r.tasks.forEach(function (t) { (cols[lv[t.id]] = cols[lv[t.id]] || []).push(t); });
        dag.textContent = "";
        cols.forEach(function (col, ci) {
          dag.appendChild(h("div", { class: "dag-col" }, h("h3", { text: ci === 0 ? "Start" : "Step " + (ci + 1) }), col.map(function (t) {
            return h("div", { class: "card task " + t.state },
              h("div", { class: "row" }, h("b", { text: t.id }), wfPill(t.state)),
              h("div", { class: "muted", text: t.node + "  -  " + t.runner }),
              h("div", { class: "cmd small", text: t.target }),
              t.needs.length ? h("div", { class: "muted", text: "after " + t.needs.join(", ") }) : null,
              t.attempts > 1 || t.retries ? h("div", { class: "muted", text: "attempt " + t.attempts + " of " + (t.retries + 1) }) : null,
              t.error ? h("div", { class: "note " + (t.state === "failed" ? "bad" : "warn"), text: t.error }) : null,
              t.proc_id ? h("a", { href: "#/proc/" + t.proc_id, text: "Open agent process #" + t.proc_id }) : null);
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
      h("thead", null, h("tr", null, ["#", "Status", "Node", "Runner", "Prompt", "Agents", "Cost", "Started"].map(function (t) { return h("th", { text: t }); }))),
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

  function termUrl(params) { return "/term?" + new URLSearchParams(params).toString(); }

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
    m.appendChild(h("div", { class: "card" },
      h("h3", { text: "Start a terminal" }),
      h("p", { class: "muted", text: "Opens in a new tab, in the directory you pick. It ends when you close the tab." }),
      targets.length ? h("table", null, h("tbody", null, targets.map(function (t, i) {
        var at = hs.targets.length ? { h: hs.name, target: i } : { h: hs.name };
        return h("tr", null, h("td", null, h("code", { text: t })), h("td", { class: "row" },
          h("a", { class: "btn", href: termUrl(Object.assign({ what: "shell" }, at)), target: "_blank", rel: "noopener" }, "Shell"),
          hasClaude ? h("a", { class: "btn", href: termUrl(Object.assign({ what: "claude" }, at)), target: "_blank", rel: "noopener" }, "Claude Code") : null));
      }))) : empty("No harness, so no directory to start in.")));
    var sessions = h("div");
    m.appendChild(h("div", { class: "card" },
      h("h3", { text: "Agent sessions" }),
      h("p", { class: "muted", text: "Open an agent's own Claude Code session interactively, in its directory. While the agent still runs, the terminal gets a branch of the session (--fork-session), so the agent carries on undisturbed." }),
      sessions));
    m.appendChild(own);
    api("/api/procs").then(function (list) {
      var ok = list.filter(function (p) { return p.node === "local" && p.session; });
      sessions.appendChild(ok.length ? h("table", null, h("tbody", null, ok.map(function (p) {
        return h("tr", null, h("td", null, h("a", { href: "#/proc/" + p.id, text: "#" + p.id })), h("td", null, statusPill(p.status)),
          h("td", { class: "muted", text: (p.prompt || "").slice(0, 80) }),
          h("td", null, h("a", { class: "btn", href: termUrl({ what: "agent", proc: p.id }), target: "_blank", rel: "noopener" }, p.status === "running" ? "Fork in terminal" : "Open in terminal")));
      }))) : empty("No local agent has reported a session yet (Claude Code does)."));
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
      note: h("p", { class: "muted small" })
    };
    [pv.cont, pv.fresh].forEach(function (r) { r.addEventListener("change", function () { pv.chosen = true; }); });
    var keepRow = h("label", { class: "check" }, pv.keep, "Keep this agent running (the continuation branches its session)");
    var pivotCard = h("form", { class: "card pivot", onsubmit: function (e) {
      e.preventDefault();
      if (!pv.text.value.trim()) { toast("Write the new direction first", true); return; }
      pv.go.disabled = true;
      api("/api/procs/" + id + "/pivot", { method: "POST", body: { prompt: pv.text.value, mode: pv.fresh.checked ? "fresh" : "continue", keep_running: pv.keep.checked } })
        .then(function (r) { location.hash = "#/proc/" + r.id; })
        .catch(function (err) { toast(err.message, true); pv.go.disabled = false; });
    } },
      h("h3", { text: "Pivot this agent" }),
      pv.text,
      h("label", { class: "check" }, pv.cont, h("span", null, h("b", { text: "Continue its session" }), " - keeps everything it learned")),
      h("label", { class: "check" }, pv.fresh, h("span", null, h("b", { text: "Start fresh" }), " - a new agent, given this one's question and result")),
      keepRow, pv.note, h("div", { class: "row" }, pv.go));
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
            if (!allow) { var why = window.prompt("Tell the agent why (optional):", ""); if (why === null) return; body.message = why; }
            api("/api/procs/" + id + "/permission", { method: "POST", body: body })
              .then(function () { toast(allow ? "Allowed" : "Denied"); refresh(); }).catch(function (e) { toast(e.message, true); });
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
        stop.disabled = true;
        api("/api/procs/" + id + "/stop", { method: "POST" }).then(function () { toast("Stopping"); }).catch(function (e) { toast(e.message, true); });
      } }, "Stop");
      var shellBtn = S.ov.shell && p.node === "local" && p.session
        ? h("a", { class: "btn", href: termUrl({ what: "agent", proc: p.id }), target: "_blank", rel: "noopener",
            title: p.status === "running" ? "Opens a branch of its session (--fork-session), so the agent is not disturbed" : "Continues its session interactively" },
            p.status === "running" ? "Fork in terminal" : "Open in terminal")
        : null;
      top.appendChild(header("Process #" + p.id + " - " + p.runner + (p.node !== "local" ? " on " + p.node : ""), (p.host && p.node !== "local" ? p.host + ":" : "") + p.cwd, [statusPill(p.status), p.cost_usd != null ? h("span", { class: "pill", text: "$" + p.cost_usd.toFixed(4) }) : null, shellBtn, stop]));
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
  var MODES = [["manual", "manual: ask before every tool"], ["acceptEdits", "acceptEdits: file edits run, the rest asks"],
    ["auto", "auto: Claude decides what is safe"], ["plan", "plan: read and plan, change nothing"],
    ["dontAsk", "dontAsk: deny whatever would ask"], ["bypassPermissions", "bypassPermissions: run everything"]];
  function modelList() {
    if ($("models")) return null;
    return h("datalist", { id: "models" }, ["opus", "sonnet", "haiku", "fable", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5", "qwen3-coder", "gpt-oss:20b"]
      .map(function (x) { return h("option", { value: x }); }));
  }

  // Whether this console may edit indra.toml (see Settings).
  function canEdit() { return !!(S.ov && S.ov.settings && S.ov.settings.editable); }
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
      jsonFile("board.json", data.items.map(function (it) { return { title: it.title, body: it.body, labels: it.labels, column: it.column, profile: it.profile, runner: it.runner, node: it.node, target: it.target, source: it.source, result: it.result }; }));
    } }, "Export");
    var importFile = h("button", { type: "button", disabled: ro, onclick: function () {
      pickFiles({ accept: ".json,application/json" }).then(function (fs) {
        if (!fs.length) return;
        var got; try { got = listFrom(fs[0].text); } catch (e) { toast(fs[0].name + ": " + e.message, true); return; }
        got = got.filter(function (x) { return x && x.title; });
        Promise.all(got.map(function (x) {
          var b = { title: String(x.title), body: x.body || "", labels: x.labels || [] };
          ["profile", "runner", "node", "target"].forEach(function (k) { if (x[k]) b[k] = x[k]; });
          if (x.column && ["todo", "review", "done"].indexOf(x.column) >= 0) b.column = x.column;
          return api("/api/board/items", { method: "POST", body: b });
        })).then(function () { toast("Imported " + got.length + " item(s)"); load(); }).catch(function (e) { toast(e.message, true); load(); });
      });
    } }, "Import file");
    m.appendChild(header("Board", "Work for agents. Run an item with a profile; when the agent finishes it moves to Review (or Failed), and you move it to Done. Items from Jira, ClickUp or GitHub report every move back.", [importBox, exportBtn, importFile, add]));
    var lanes = h("div", { class: "lanes" }), detail = h("div");
    m.appendChild(lanes); m.appendChild(detail);
    var data = null, open = null;
    function patch(id, body) {
      return api("/api/board/items/" + id, { method: "PATCH", body: body }).then(load).catch(function (e) { toast(e.message, true); });
    }
    function runForm(it) {
      var prof = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: it.profile ? "item: " + it.profile : "connector or runner default" }),
        (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); }));
      var node = h("select", { "aria-label": "Node" }, ov.nodes.map(function (n) { return h("option", { value: n, text: n }); }));
      node.value = it.node || "local";
      var target = h("input", { type: "text", placeholder: it.target || "the harness's first target", "aria-label": "Target" });
      var go = h("button", { class: "primary", type: "submit" }, "Run");
      return h("form", { class: "row runform", onsubmit: function (e) {
        e.preventDefault(); go.disabled = true;
        api("/api/board/items/" + it.id + "/run", { method: "POST", body: { profile: prof.value || null, node: node.value, target: target.value.trim() || null } })
          .then(function (r) { toast("Agent process #" + r.proc + " started"); load(); }).catch(function (err) { toast(err.message, true); go.disabled = false; });
      } }, prof, node, target, go);
    }
    function editItem(it) {
      var f = { title: h("input", { type: "text", value: it.title, required: true }), body: h("textarea", null),
        labels: h("input", { type: "text", value: (it.labels || []).join(", "), placeholder: "comma separated" }),
        profile: h("select", null, h("option", { value: "", text: "none" }), (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); })),
        target: h("input", { type: "text", value: it.target || "", placeholder: "the harness's first target" }) };
      f.body.value = it.body || ""; f.profile.value = it.profile || "";
      detail.textContent = "";
      detail.appendChild(h("form", { class: "form card", onsubmit: function (e) {
        e.preventDefault();
        api("/api/board/items/" + it.id, { method: "PATCH", body: { title: f.title.value, body: f.body.value, labels: csv(f.labels.value), profile: f.profile.value, target: f.target.value.trim() } })
          .then(function (x) { toast("Saved #" + it.id); load(); showDetail(x); }).catch(function (err) { toast(err.message, true); });
      } }, h("h3", { text: "Edit #" + it.id }), h("label", null, "Title", f.title), h("label", null, "Description", f.body), h("label", null, "Labels", f.labels),
        h("div", { class: "row" }, h("label", null, "Profile", f.profile), h("label", null, "Target", f.target)),
        it.source ? h("p", { class: "muted small", text: "Changes stay on the board; the item in " + it.source.connector + " is not edited." }) : null,
        h("div", { class: "row" }, h("button", { type: "submit", class: "primary" }, "Save"), h("button", { type: "button", onclick: function () { showDetail(it); } }, "Cancel"))));
    }
    function showDetail(it) {
      open = it ? it.id : null; detail.textContent = "";
      if (!it) return;
      var src = it.source;
      detail.appendChild(h("div", { class: "card itemdetail" },
        h("div", { class: "row" }, h("h2", { text: "#" + it.id + " " + it.title }), src ? h("a", { href: src.url, target: "_blank", rel: "noopener", text: src.connector + " " + src.key + (src.state ? " (" + src.state + ")" : "") }) : null,
          h("button", { type: "button", class: "ghost", onclick: function () { showDetail(null); } }, "Close")),
        it.body ? h("pre", { class: "view small", text: it.body }) : h("p", { class: "muted", text: "No description." }),
        it.column !== "running" && !ro ? runForm(it) : null,
        it.proc_id ? h("p", null, h("a", { href: "#/proc/" + it.proc_id, text: "Open agent process #" + it.proc_id })) : null,
        it.result ? [h("h3", { text: "Result" }), h("pre", { class: "view small", text: it.result })] : null,
        it.notes.length ? [h("h3", { text: "History" }), h("ul", { class: "links" }, it.notes.slice().reverse().map(function (n) { return h("li", null, h("span", { class: "muted", text: ago(n.ts) + "  " }), n.text); }))] : null,
        ro || it.column === "running" ? null : h("div", { class: "row" }, h("button", { type: "button", onclick: function () { editItem(it); } }, "Edit"), h("button", { type: "button", class: "danger", onclick: function () {
          if (!window.confirm("Remove #" + it.id + " from the board? (Its tracker item is not touched.)")) return;
          api("/api/board/items/" + it.id, { method: "DELETE" }).then(function () { showDetail(null); load(); }).catch(function (e) { toast(e.message, true); });
        } }, "Remove from board"))));
      detail.scrollIntoView({ behavior: "smooth", block: "nearest" });
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
      lanes.textContent = "";
      COLS.forEach(function (col) {
        var items = data.items.filter(function (i) { return i.column === col[0]; });
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
      data.connectors.forEach(function (c) {
        importBox.appendChild(h("button", { type: "button", disabled: ro, title: c.kind + " " + c.project + (c.auto_run ? ", runs new items automatically" : ""), onclick: function (e) {
          e.target.disabled = true;
          api("/api/board/import", { method: "POST", body: { connector: c.name } })
            .then(function (r) { toast(c.name + ": " + r.added + " new, " + r.updated + " updated"); load(); })
            .catch(function (err) { toast(err.message, true); }).then(function () { e.target.disabled = false; });
        } }, "Import from " + c.name));
      });
      if (!data.connectors.length && canEdit()) importBox.appendChild(h("a", { href: "#/settings/connectors", text: "Connect Jira, ClickUp or GitHub" }));
      if (open) { var it = data.items.find(function (x) { return x.id === open; }); if (it) showDetail(it); }
    }
    function load() { return api("/api/board").then(function (d) { data = d; render(); }).catch(function (e) { lanes.textContent = e.message; }); }
    add.addEventListener("click", function () {
      var title = h("input", { type: "text", required: true, "aria-label": "Title", placeholder: "What should an agent do?" });
      var body = h("textarea", { "aria-label": "Details", placeholder: "Details, links, acceptance criteria" });
      var prof = h("select", { "aria-label": "Profile" }, h("option", { value: "", text: "No profile" }), (ov.profiles || []).map(function (p) { return h("option", { value: p.name, text: p.name }); }));
      detail.textContent = "";
      detail.appendChild(h("form", { class: "form card", onsubmit: function (e) {
        e.preventDefault();
        api("/api/board/items", { method: "POST", body: { title: title.value, body: body.value, profile: prof.value || null } })
          .then(function () { detail.textContent = ""; load(); }).catch(function (err) { toast(err.message, true); });
      } }, h("h2", { text: "New item" }), h("label", null, "Title", title), h("label", null, "Details", body), h("label", null, "Profile", prof),
        h("div", { class: "row" }, h("button", { class: "primary", type: "submit" }, "Add to To do"), h("button", { type: "button", onclick: function () { detail.textContent = ""; } }, "Cancel"))));
      title.focus();
    });
    load(); every(3000, function () { if (!document.querySelector(".lane.over")) load(); });
  }

  // ---- workflow editor: a graph of tasks, dragged and connected -------------------
  // Tasks are boxes; an arrow from A to B means B runs after A (B.needs has A).
  // The palette adds blank tasks, components (reusable tasks) and other
  // workflows (run as one step). Saving writes ordinary [[workflow]] TOML,
  // positions included, so `indra run` and the text editor keep working.
  function pageWorkflowEditor(m, name) {
    var c = null, wf = null, original = name || null, sel = null, selEdge = null, dirty = false;
    var palette = h("div", { class: "palette", "aria-label": "Add to the workflow" });
    var scroller = h("div", { class: "canvas-scroll" });
    var canvas = h("div", { class: "canvas", role: "application", "aria-label": "Workflow graph" });
    var svg = document.createElementNS("http://www.w3.org/2000/svg", "svg"); svg.setAttribute("class", "edges");
    canvas.appendChild(svg); scroller.appendChild(canvas);
    var inspector = h("div", { class: "inspector card" });
    var status = h("span", { class: "muted small" });
    var save = h("button", { class: "primary", type: "button" }, "Save");
    var layoutBtn = h("button", { type: "button" }, "Tidy layout");
    m.appendChild(header(original ? "Edit workflow " + original : "New workflow", "Drag tasks, components and workflows in from the left. Drag from a task's right edge to another task to make the second run after the first. Select an arrow and press Delete to remove it.", [status, layoutBtn, save]));
    m.appendChild(h("div", { class: "wfedit" }, palette, scroller, inspector));

    function changed() { dirty = true; status.textContent = "Unsaved changes"; }
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
    function kind(t) { return t.workflow ? "workflow " + t.workflow : t.component ? "component " + t.component : "task"; }

    function draw() {
      Array.prototype.slice.call(canvas.querySelectorAll(".gnode")).forEach(function (n) { n.remove(); });
      while (svg.firstChild) svg.removeChild(svg.firstChild);
      var maxX = 900, maxY = 500;
      wf.task.forEach(function (t) {
        var n = h("div", { class: "gnode" + (sel === t.id ? " sel" : "") + (t.workflow ? " sub" : t.component ? " comp" : ""), tabindex: "0", "data-id": t.id },
          h("span", { class: "port in", title: "runs after" }),
          h("b", { text: t.id }), h("small", { text: kind(t) }),
          h("small", { class: "muted", text: [t.node || "local", t.profile || t.runner || ""].filter(Boolean).join(" - ") }),
          h("span", { class: "port out", title: "drag to the task that runs after this one" }));
        n.style.left = t.pos[0] + "px"; n.style.top = t.pos[1] + "px";
        canvas.appendChild(n);
        maxX = Math.max(maxX, t.pos[0] + 400); maxY = Math.max(maxY, t.pos[1] + 260);
        n.addEventListener("pointerdown", function (e) {
          if (e.target.classList.contains("out")) return connect(e, t);
          select(t.id); e.preventDefault();
          var sx = e.clientX, sy = e.clientY, ox = t.pos[0], oy = t.pos[1];
          try { n.setPointerCapture(e.pointerId); } catch (x) { /* a synthetic or ended pointer */ }
          function mv(ev) { t.pos = [Math.max(0, Math.round(ox + ev.clientX - sx)), Math.max(0, Math.round(oy + ev.clientY - sy))]; n.style.left = t.pos[0] + "px"; n.style.top = t.pos[1] + "px"; edges(); }
          function up() { n.removeEventListener("pointermove", mv); n.removeEventListener("pointerup", up); if (t.pos[0] !== ox || t.pos[1] !== oy) changed(); }
          n.addEventListener("pointermove", mv); n.addEventListener("pointerup", up);
        });
        n.addEventListener("keydown", function (e) { if (e.key === "Delete" || e.key === "Backspace") { removeTask(t.id); e.preventDefault(); } });
      });
      canvas.style.width = maxX + "px"; canvas.style.height = maxY + "px";
      svg.setAttribute("width", maxX); svg.setAttribute("height", maxY);
      edges();
    }
    function center(id, side) {
      var n = canvas.querySelector('.gnode[data-id="' + CSS.escape(id) + '"]'); if (!n) return null;
      return { x: n.offsetLeft + (side === "out" ? n.offsetWidth : 0), y: n.offsetTop + n.offsetHeight / 2 };
    }
    function path(a, b) { var dx = Math.max(40, Math.abs(b.x - a.x) / 2); return "M" + a.x + "," + a.y + " C" + (a.x + dx) + "," + a.y + " " + (b.x - dx) + "," + b.y + " " + b.x + "," + b.y; }
    function edges() {
      while (svg.firstChild) svg.removeChild(svg.firstChild);
      wf.task.forEach(function (t) {
        (t.needs || []).forEach(function (from) {
          var a = center(from, "out"), b = center(t.id, "in"); if (!a || !b) return;
          var on = selEdge && selEdge[0] === from && selEdge[1] === t.id;
          var hit = document.createElementNS("http://www.w3.org/2000/svg", "path");
          hit.setAttribute("d", path(a, b)); hit.setAttribute("class", "hit");
          hit.addEventListener("pointerdown", function (e) { e.stopPropagation(); selEdge = [from, t.id]; sel = null; inspect(); edges(); });
          var p = document.createElementNS("http://www.w3.org/2000/svg", "path");
          p.setAttribute("d", path(a, b)); p.setAttribute("class", "edge" + (on ? " on" : ""));
          svg.appendChild(p); svg.appendChild(hit);
        });
      });
    }
    function connect(e, from) {
      e.preventDefault(); e.stopPropagation();
      var line = document.createElementNS("http://www.w3.org/2000/svg", "path"); line.setAttribute("class", "edge draft"); svg.appendChild(line);
      var a = center(from.id, "out"), box = canvas.getBoundingClientRect();
      function mv(ev) { line.setAttribute("d", path(a, { x: ev.clientX - box.left, y: ev.clientY - box.top })); }
      function up(ev) {
        document.removeEventListener("pointermove", mv); document.removeEventListener("pointerup", up); line.remove();
        var el = document.elementFromPoint(ev.clientX, ev.clientY), n = el && el.closest(".gnode");
        if (!n) return;
        var to = task(n.getAttribute("data-id"));
        if (!to || to.id === from.id) return;
        if ((to.needs || []).indexOf(from.id) >= 0) return;
        if (reaches(to.id, from.id)) { toast("That would make a loop: " + from.id + " already runs after " + to.id, true); return; }
        to.needs = (to.needs || []).concat([from.id]); changed(); draw(); select(to.id);
      }
      document.addEventListener("pointermove", mv); document.addEventListener("pointerup", up);
    }
    function removeTask(id) {
      wf.task = wf.task.filter(function (t) { return t.id !== id; });
      wf.task.forEach(function (t) { t.needs = (t.needs || []).filter(function (n) { return n !== id; }); });
      sel = null; changed(); draw(); inspect();
    }
    function select(id) { sel = id; selEdge = null; Array.prototype.forEach.call(canvas.querySelectorAll(".gnode"), function (n) { n.classList.toggle("sel", n.getAttribute("data-id") === id); }); edges(); inspect(); }
    document.addEventListener("keydown", function onKey(e) {
      if (!document.body.contains(canvas)) { document.removeEventListener("keydown", onKey); return; }
      if (selEdge && (e.key === "Delete" || e.key === "Backspace") && !/INPUT|TEXTAREA|SELECT/.test(document.activeElement.tagName)) {
        var t = task(selEdge[1]); if (t) t.needs = t.needs.filter(function (n) { return n !== selEdge[0]; });
        selEdge = null; changed(); draw(); inspect(); e.preventDefault();
      }
    });
    scroller.addEventListener("pointerdown", function (e) { if (e.target === canvas || e.target === svg || e.target === scroller) { sel = null; selEdge = null; draw(); inspect(); } });

    function field(label, el, help) { return h("label", null, label, help ? h("small", { text: help }) : null, el); }
    function inspect() {
      inspector.textContent = "";
      if (selEdge) {
        inspector.appendChild(h("h3", { text: "Link" }));
        inspector.appendChild(h("p", { text: selEdge[1] + " runs after " + selEdge[0] + "." }));
        inspector.appendChild(h("button", { type: "button", class: "danger", onclick: function () {
          var t = task(selEdge[1]); t.needs = t.needs.filter(function (n) { return n !== selEdge[0]; }); selEdge = null; changed(); draw(); inspect();
        } }, "Remove link"));
        return;
      }
      var t = sel && task(sel);
      if (!t) {
        var nm = h("input", { type: "text", value: wf.name, required: true }), ds = h("input", { type: "text", value: wf.description || "" });
        nm.addEventListener("input", function () { wf.name = nm.value.trim(); changed(); });
        ds.addEventListener("input", function () { wf.description = ds.value; changed(); });
        inspector.appendChild(h("div", { class: "form" }, h("h3", { text: "Workflow" }), field("Name", nm, "Run it with `indra run <name>` or from the Workflows page."), field("Description", ds),
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
      f.appendChild(h("h3", { text: t.workflow ? "Sub-workflow step" : "Task" }));
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
      if (t.workflow) {
        f.appendChild(field("Runs workflow", sel_("workflow", (c.config.workflow || []).map(function (w) { return w.name; }).filter(function (n) { return n !== wf.name; }), null),
          "Its tasks run as this one step: they start after this step's links, and what runs after this step waits for them all."));
      } else {
        var comp = c.config.component || [], cc = t.component && comp.find(function (x) { return x.name === t.component; });
        f.appendChild(field("Component", sel_("component", comp.map(function (x) { return x.name; }), "None"), "Fields left empty come from the component."));
        f.appendChild(field("Node", sel_("node", cfgNodeNames(c), cc && cc.node ? "component: " + cc.node : "local")));
        f.appendChild(field("Profile", sel_("profile", cfgProfileNames(c), cc && cc.profile ? "component: " + cc.profile : "None")));
        f.appendChild(field("Runner", sel_("runner", cfgRunnerNames(c), cc && cc.runner ? "component: " + cc.runner : "claude (or the profile's)")));
        f.appendChild(field("Target", text("target", false, cc && cc.target ? cc.target : "a directory on that node")));
        f.appendChild(field("Prompt", text("prompt", true, cc && cc.prompt ? cc.prompt : "What should the agent do? {target} and {node} are filled in.")));
        f.appendChild(field("Retries", text("retries", false, cc && cc.retries ? String(cc.retries) : "0")));
      }
      f.appendChild(h("p", { class: "muted small", text: (t.needs || []).length ? "Runs after: " + t.needs.join(", ") : "Runs first (nothing before it)." }));
      f.appendChild(h("div", { class: "row" },
        !t.workflow && canEdit() ? h("button", { type: "button", onclick: function () {
          var n = window.prompt("Save this task as a reusable component named:", t.id); if (!n) return;
          var comp = { name: n.trim() }; ["prompt", "profile", "runner", "node", "target"].forEach(function (k) { if (t[k]) comp[k] = t[k]; }); if (t.retries) comp.retries = t.retries;
          addToSection("component", comp).then(function (r) { c = r; toast("Saved component " + comp.name); renderPalette(); }).catch(function (e) { toast(e.message, true); });
        } }, "Save as component") : null,
        h("button", { type: "button", class: "danger", onclick: function () { removeTask(t.id); } }, "Remove step")));
      inspector.appendChild(f);
    }

    function add(spec, x, y) {
      var t = { id: uniqueId(spec.workflow || spec.component || "task"), pos: [Math.round(x), Math.round(y)] };
      if (spec.workflow) t.workflow = spec.workflow;
      if (spec.component) t.component = spec.component;
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
      var comps = c.config.component || [];
      palette.appendChild(h("h3", { text: "Components" }));
      if (!comps.length) palette.appendChild(h("p", { class: "muted small", text: "None yet. Select a task and press Save as component to reuse it." }));
      comps.forEach(function (x) { palette.appendChild(item(x.name, x.description || x.prompt, { component: x.name })); });
      var others = (c.config.workflow || []).filter(function (w) { return w.name !== original; });
      palette.appendChild(h("h3", { text: "Workflows" }));
      if (!others.length) palette.appendChild(h("p", { class: "muted small", text: "Other workflows appear here, to run as one step." }));
      others.forEach(function (w) { palette.appendChild(item(w.name, (w.task || []).length + " steps", { workflow: w.name })); });
    }
    scroller.addEventListener("dragover", function (e) { e.preventDefault(); });
    scroller.addEventListener("drop", function (e) {
      var d = e.dataTransfer.getData("application/x-indra"); if (!d) return;
      e.preventDefault(); var box = canvas.getBoundingClientRect();
      add(JSON.parse(d), e.clientX - box.left - 90, e.clientY - box.top - 30);
    });
    layoutBtn.addEventListener("click", function () { tidy(true); changed(); draw(); });
    save.addEventListener("click", function () {
      if (!wf.name || !/^[A-Za-z0-9_-]{1,64}$/.test(wf.name)) { toast("Give the workflow a name: letters, digits, - and _", true); return; }
      if (!wf.task.length) { toast("Add at least one step", true); return; }
      var clean = JSON.parse(JSON.stringify(wf));
      if (!clean.description) delete clean.description;
      clean.task.forEach(function (t) { if (!t.needs || !t.needs.length) delete t.needs; if (!t.retries) delete t.retries; });
      var list = (c.config.workflow || []).filter(function (w) { return w.name !== original && w.name !== clean.name; });
      var at = (c.config.workflow || []).findIndex(function (w) { return w.name === original; });
      if (at >= 0 && at <= list.length) list.splice(at, 0, clean); else list.push(clean);
      save.disabled = true;
      api("/api/config", { method: "PUT", body: { version: c.version, section: "workflow", value: list } }).then(function (r) {
        c = r; original = clean.name; dirty = false; status.textContent = "Saved"; toast("Saved workflow " + clean.name + ". Applied now.");
        var title = m.querySelector(".head h1"); if (title) title.textContent = "Edit workflow " + clean.name; renderPalette();
        refreshOverview(); if (location.hash !== "#/wfedit/" + encodeURIComponent(clean.name)) history.replaceState(null, "", "#/wfedit/" + encodeURIComponent(clean.name));
      }).catch(function (e) { toast(e.message, true); status.textContent = "Not saved: " + e.message; }).then(function () { save.disabled = !canEdit(); });
    });
    window.addEventListener("beforeunload", function warn(e) { if (!document.body.contains(canvas)) { window.removeEventListener("beforeunload", warn); return; } if (dirty) { e.preventDefault(); e.returnValue = ""; } });

    api("/api/config").then(function (x) {
      c = x;
      if (c.error) { inspector.textContent = "indra.toml has an error; fix it in Settings > File first."; return; }
      var found = original && (c.config.workflow || []).find(function (w) { return w.name === original; });
      wf = found ? JSON.parse(JSON.stringify(found)) : { name: original || "new-workflow", task: [] };
      wf.task = wf.task || [];
      tidy(false); renderPalette(); draw(); inspect();
      save.disabled = !c.editable; status.textContent = c.editable ? "" : "Read-only: " + c.locked;
    }).catch(function (e) { inspector.textContent = e.status === 404 ? "This console cannot edit indra.toml." : e.message; });
  }

  // ---- settings: indra.toml, edited here ------------------------------------------
  // Each section of the config file has a schema; forms are built from it, and
  // a save replaces that one section on the server, which keeps the rest of
  // the file (comments included), checks the result, and applies it at once.
  var TABS = [["general", "General"], ["profiles", "Profiles"], ["runners", "Runners"], ["nodes", "Nodes"], ["workflows", "Workflows"], ["components", "Components"],
    ["presets", "Presets"], ["harnesses", "Harnesses"], ["connectors", "Connectors"], ["secrets", "Secrets"], ["file", "File"]];

  function opts(list, blank) { return (blank ? [["", blank]] : []).concat(list.map(function (x) { return [x, x]; })); }
  function cfgRunnerNames(c) {
    var n = {}; (c.config.runner || []).concat(c.builtin.runners).forEach(function (r) { n[r.name] = 1; }); return Object.keys(n);
  }
  function cfgHarnessNames(c) {
    var n = {}; (c.config.harness || []).concat(c.builtin.harnesses).forEach(function (x) { n[x.name] = 1; }); return Object.keys(n);
  }
  function cfgNodeNames(c) { return ["local"].concat((c.config.node || []).map(function (n) { return n.name; })); }
  function cfgProfileNames(c) { return (c.config.profile || []).map(function (p) { return p.name; }); }
  // Starting points for "Add profile".
  var PROFILE_TEMPLATES = [
    { label: "Planner (Opus, plan mode)", v: { name: "planner", description: "Reads and plans; changes nothing", runner: "claude", model: "opus", permission_mode: "plan", effort: "high" } },
    { label: "Worker (Sonnet, accept edits)", v: { name: "worker", description: "Does the work the planner laid out", runner: "claude", model: "sonnet", permission_mode: "acceptEdits",
      allowed_tools: ["Read", "Grep", "Glob", "Edit", "Write", "Bash(git diff:*)", "Bash(git status:*)"] } },
    { label: "Local model (Ollama)", v: { name: "local", description: "Claude Code against a local model served by Ollama", runner: "claude", model: "qwen3-coder",
      permission_mode: "manual", env: { ANTHROPIC_BASE_URL: "http://localhost:11434", ANTHROPIC_AUTH_TOKEN: "ollama", ANTHROPIC_API_KEY: "" } } }
  ];

  var SCHEMA = {
    component: { tab: "components", noun: "component", fields: function (c) { return [
      { k: "name", label: "Name", req: true, help: "Drag it into a workflow from the editor's palette; a task's own fields override it." },
      { k: "description", label: "Description" },
      { k: "prompt", label: "Prompt", type: "textarea", help: "{target} and {node} are filled in." },
      { k: "profile", label: "Profile", type: "select", options: opts(cfgProfileNames(c), "None") },
      { k: "runner", label: "Runner", type: "select", options: opts(cfgRunnerNames(c), "Default") },
      { k: "node", label: "Node", type: "select", options: opts(cfgNodeNames(c), "Default (local)") },
      { k: "target", label: "Target" },
      { k: "retries", label: "Retries", type: "number", placeholder: "0" }]; },
      summary: function (x) { return x.prompt || x.description || ""; } },
    connector: { tab: "connectors", noun: "connector", fields: function (c) { return [
      { k: "name", label: "Name", req: true },
      { k: "kind", label: "Tracker", type: "select", req: true, options: [["jira", "Jira (Cloud)"], ["github", "GitHub Issues"], ["clickup", "ClickUp"]] },
      { k: "url", label: "Site URL", placeholder: "https://acme.atlassian.net", help: "Jira: your site. GitHub and ClickUp: leave empty for the public API (set it for GitHub Enterprise)." },
      { k: "project", label: "Project", req: true, help: "Jira project key (SEC), GitHub owner/repo, or ClickUp list id." },
      { k: "query", label: "Filter", help: "Jira: extra JQL (labels = agent). GitHub: a label. ClickUp: statuses, comma-separated." },
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
      { k: "name", label: "Name", req: true, help: "Pick it on Launch, in presets, in workflow tasks and on the board." },
      { k: "description", label: "Description" },
      { k: "runner", label: "Runner", type: "select", options: opts(cfgRunnerNames(c), "Whatever Launch has selected") },
      { k: "model", label: "Model", list: "models", placeholder: "default", help: "An alias (opus, sonnet, haiku) or a full model id; for Ollama, the local model's name." },
      { k: "permission_mode", label: "Permission mode", type: "select", options: [["", "default"]].concat(MODES) },
      { k: "effort", label: "Effort", type: "select", options: opts(["low", "medium", "high", "xhigh", "max"], "default") },
      { k: "allowed_tools", label: "Always allowed tools", type: "list", help: "One rule per line, e.g. Read, Edit, Bash(git diff:*). These never ask." },
      { k: "disallowed_tools", label: "Never allowed tools", type: "list", help: "One rule per line, e.g. Bash(rm:*), WebFetch." },
      { k: "env", label: "Environment", type: "kv", help: "KEY=value per line, e.g. ANTHROPIC_BASE_URL=http://localhost:11434 for a local model through Ollama. For a token, write KEY=secret:<name> and set it in the Secrets tab: the value never goes in indra.toml." },
      { k: "memory", label: "Start with the memory notes most relevant to the prompt", type: "bool" },
      { k: "reflect", label: "Before finishing, propose learnings for review (Claude Code)", type: "bool" }]; },
      summary: function (p) { return [p.runner || "any runner", p.model || "default model", p.permission_mode || "default mode"].join(", ") + (p.env && Object.keys(p.env).length ? ", env: " + Object.keys(p.env).join(" ") : "") + (p.memory ? ", memory" : "") + (p.reflect ? ", reflects" : ""); } },
    runner: { tab: "runners", noun: "runner", fields: function () { return [
      { k: "name", label: "Name", req: true, help: "How you pick it on Launch and in workflows. A built-in name (claude, codex, gemini) replaces the built-in." },
      { k: "command", label: "Program", req: true, help: "A program on PATH or a full path. It runs directly, never through a shell." },
      { k: "args", label: "Arguments", type: "list", req: true, help: "One per line. {prompt} is required; {target} is the directory the agent runs in. Each is filled in as one whole argument." },
      { k: "format", label: "Output", type: "select", options: [["", "Plain text (default)"], ["text", "Plain text"], ["claude-stream-json", "Claude Code stream-json: subagents, tool calls, cost"]] },
      { k: "resume_args", label: "Arguments to continue a session", type: "list", help: "Used by Pivot > Continue; {session} is the session id. stream-json runners default to --resume {session}." },
      { k: "fork_args", label: "Arguments to branch a session", type: "list", help: "Added after those when the original agent keeps running, e.g. --fork-session." }]; },
      summary: function (r) { return r.command + " " + (r.args || []).join(" "); } },
    node: { tab: "nodes", noun: "node", fields: function () { return [
      { k: "name", label: "Name", req: true },
      { k: "description", label: "Description" },
      { k: "ssh", label: "SSH destination", req: true, help: "host, user@host, or a Host alias from ~/.ssh/config. `ssh <destination>` must work without a password prompt, and the host key must already be trusted." },
      { k: "targets", label: "Targets", type: "list", help: "Codebases on that machine, one path per line, as paths there." },
      { k: "max_parallel", label: "Agents at once", type: "number", placeholder: "2" },
      { k: "port", label: "SSH port", type: "number", placeholder: "22" },
      { k: "identity", label: "Private key file", placeholder: "whatever ssh would use" },
      { k: "indra", label: "indra on the remote", placeholder: "indra" },
      { k: "args", label: "Extra arguments for the remote indra", type: "list", help: "One per line, e.g. --config and /etc/indra.toml." },
      { k: "ssh_command", label: "ssh client", placeholder: "ssh" }]; },
      summary: function (n) { return n.ssh + "  -  " + (n.targets || []).length + " targets, " + (n.max_parallel || 2) + " agents at once"; } },
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
        { k: "target", label: "Target", req: true, help: "A directory on that node. Local paths are relative to the config file." },
        { k: "prompt", label: "Prompt", type: "textarea", req: true, help: "{target} and {node} are filled in." },
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
      var row = { ctl: ctl, box: box };
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
      var out = rows.map(function (r) { var o = {}; r.ctl.forEach(function (x) { var v = x.c.get(); if (v !== undefined) o[x.k] = v; }); return o; });
      return out.length ? out : undefined;
    } };
  }

  function itemForm(fields, item, title, onSave, onCancel) {
    var ctl = fields.map(function (f) { return { k: f.k, c: control(f, item[f.k]) }; });
    var err = h("div", { class: "note bad", hidden: true, role: "alert" });
    var save = h("button", { class: "primary", type: "submit" }, "Save");
    var form = h("form", { class: "form card cfgform", onsubmit: function (e) {
      e.preventDefault();
      var o = {}; ctl.forEach(function (x) { var v = x.c.get(); if (v !== undefined) o[x.k] = v; });
      save.disabled = true; err.hidden = true;
      onSave(o).catch(function (ex) { err.textContent = ex.message; err.hidden = false; save.disabled = false; });
    } }, h("h2", { text: title }), ctl.map(function (x) { return x.c.el; }), err,
      h("div", { class: "row" }, save, h("button", { type: "button", onclick: onCancel }, "Cancel")));
    return form;
  }

  function pageSettings(m, tab) {
    tab = TABS.some(function (t) { return t[0] === tab; }) ? tab : "general";
    m.appendChild(header("Settings", "Everything in indra.toml, edited here. Each save is checked first and applies at once; agents already running are not affected."));
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
      ({ general: general, profiles: function () { list("profile"); }, runners: function () { list("runner", c.builtin.runners); }, nodes: function () { list("node"); },
        workflows: function () { list("workflow"); }, presets: function () { list("preset", c.builtin.presets); },
        harnesses: function () { list("harness", c.builtin.harnesses); }, connectors: function () { list("connector"); }, components: function () { list("component"); }, secrets: secrets, file: file })[tab]();
    }

    function general() {
      var cf = c.config, fields = [
        { k: "builtins", label: "Show the built-in profiles (the indra plugin, and Claude Code projects)", type: "bool" },
        { k: "max_parallel", label: "Agents at once on this machine", type: "number", placeholder: "4", help: "Workflows and fan-outs wait for room; a manual Launch always starts." },
        { k: "bind", label: "Listen on", placeholder: "127.0.0.1:7878", help: "Used the next time indra serve starts (now: " + c.startup.bind + "). Keep it on loopback." },
        { k: "read_only", label: "Start read-only (no launching, no editing)", type: "bool", help: "Used the next time indra serve starts." }];
      var vals = { builtins: cf.builtins !== false, max_parallel: cf.local.max_parallel, bind: cf.server.bind, read_only: !!cf.server.read_only };
      var form = itemForm(fields, vals, "General", function (o) {
        // One section at a time; each save returns the version the next one needs.
        return saveSection("builtins", o.builtins ? null : false, "Saved")
          .then(function () { return saveSection("local", o.max_parallel === undefined ? {} : { max_parallel: o.max_parallel }); })
          .then(function () { var sv = {}; if (o.bind) sv.bind = o.bind; if (o.read_only) sv.read_only = true; return saveSection("server", sv); });
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
      var add = h("button", { class: "primary", type: "button", disabled: !c.editable, onclick: function () { edit(null, {}, "New " + d.noun); } }, "Add " + d.noun);
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
      body.appendChild(h("div", { class: "row cfgbar" }, add, section === "profile" ? PROFILE_TEMPLATES.map(function (t) {
        return h("button", { type: "button", disabled: !c.editable, onclick: function () { edit(null, JSON.parse(JSON.stringify(t.v)), "New profile: " + t.label); } }, "+ " + t.label);
      }) : null, exportBtn, importBtn));
      if (section === "profile") body.appendChild(h("p", { class: "muted", text: "A profile is how an agent runs: plan with a strong model, do the work with the one you choose, or try it on a local model. Launch, presets, workflow tasks and fan-outs can pick one; model and mode can still change while the agent runs." }));
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
      $("version").textContent = "v" + ov.version;
      $("ro-badge").hidden = !ov.read_only;
      var b = $("running-badge"); b.hidden = !ov.running; b.textContent = String(ov.running);
      var a = $("approvals-badge"); a.hidden = !ov.approvals; a.textContent = String(ov.approvals || 0);
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
    var navKey = { def: "agents", run: "runs", proc: "procs", workflow: "workflows", wfedit: "workflows", memory: "knowledge" }[page] || page;
    Array.prototype.forEach.call(document.querySelectorAll("#nav a"), function (a) {
      var on = a.dataset.route === navKey; a.classList.toggle("on", on);
      if (on) a.setAttribute("aria-current", "page"); else a.removeAttribute("aria-current");
    });
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
        proc: function () { pageProc(m, arg); },
        settings: function () { pageSettings(m, arg); },
        board: function () { pageBoard(m); },
        wfedit: function () { pageWorkflowEditor(m, arg); },
        terminal: function () { pageTerminal(m); }
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
