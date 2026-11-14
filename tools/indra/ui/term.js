// The terminal page: a PTY on the console's machine, over a WebSocket.
// Keystrokes go up as binary frames, resizes as {"t":"resize"} text frames,
// and the program's output comes back as binary frames.
(function () {
  "use strict";
  var q = new URLSearchParams(location.search);
  var params = new URLSearchParams();
  ["what", "h", "target", "proc"].forEach(function (k) { if (q.get(k) !== null) params.set(k, q.get(k)); });
  if (!params.get("what")) params.set("what", "shell");

  function $(id) { return document.getElementById(id); }
  function saved(k) { try { return localStorage.getItem(k); } catch (e) { return null; } }
  var t = saved("indra.theme");
  if (t === "light" || t === "dark") document.documentElement.setAttribute("data-theme", t);
  var dark = t === "dark" || (t !== "light" && window.matchMedia && matchMedia("(prefers-color-scheme: dark)").matches);
  var css = getComputedStyle(document.documentElement);
  function v(name, fallback) { return (css.getPropertyValue(name) || "").trim() || fallback; }

  var term = new Terminal({
    cursorBlink: true,
    fontSize: 13,
    fontFamily: "ui-monospace, SFMono-Regular, Menlo, Consolas, monospace",
    scrollback: 5000,
    theme: {
      background: v("--panel", dark ? "#1b1b1a" : "#ffffff"),
      foreground: v("--fg", dark ? "#ececea" : "#1c1c1a"),
      cursor: v("--accent", "#2f6fdf"),
      selectionBackground: dark ? "#3a4a6a" : "#cfdcf5"
    }
  });
  var fit = new FitAddon.FitAddon();
  term.loadAddon(fit);
  term.open($("term"));
  fit.fit();
  term.focus();

  var ws = null;
  var enc = new TextEncoder();
  function state(text, cls) { var s = $("state"); s.textContent = text; s.className = "pill " + (cls || ""); }
  function send(data) { if (ws && ws.readyState === 1) ws.send(data); }
  term.onData(function (d) { send(enc.encode(d)); });
  term.onBinary(function (d) {
    var b = new Uint8Array(d.length);
    for (var i = 0; i < d.length; i++) b[i] = d.charCodeAt(i) & 255;
    send(b);
  });
  term.onResize(function (sz) { send(JSON.stringify({ t: "resize", cols: sz.cols, rows: sz.rows })); });
  window.addEventListener("resize", function () { try { fit.fit(); } catch (e) { /* hidden */ } });

  function start() {
    $("again").hidden = true;
    state("starting", "");
    fetch("/api/term/check?" + params.toString(), { credentials: "same-origin" })
      .then(function (r) { return r.json().then(function (j) { return { ok: r.ok, j: j }; }); })
      .then(function (r) {
        if (!r.ok) throw new Error(r.j.error || "refused");
        $("what").textContent = r.j.label;
        document.title = r.j.label + " - indra";
        var u = new URL("/api/term?" + params.toString(), location.href);
        u.protocol = location.protocol === "https:" ? "wss:" : "ws:";
        ws = new WebSocket(u.toString());
        ws.binaryType = "arraybuffer";
        ws.onopen = function () {
          state("connected", "ok");
          send(JSON.stringify({ t: "resize", cols: term.cols, rows: term.rows }));
          term.focus();
        };
        ws.onmessage = function (e) {
          term.write(typeof e.data === "string" ? e.data : new Uint8Array(e.data));
        };
        ws.onclose = function () {
          state("ended", "warn");
          $("again").hidden = false;
          ws = null;
        };
      })
      .catch(function (e) {
        state("unavailable", "bad");
        term.write("\r\n" + String(e.message || e) + "\r\n");
      });
  }
  $("again").addEventListener("click", function () { term.reset(); start(); });
  start();
})();
