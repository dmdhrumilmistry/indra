// Copy buttons for the code samples. Everything else on the page is static.
document.querySelectorAll("button.copy").forEach(function (b) {
  b.addEventListener("click", function () {
    var text = b.getAttribute("data-copy") || "";
    var done = function () { b.textContent = "Copied"; setTimeout(function () { b.textContent = "Copy"; }, 1600); };
    try {
      navigator.clipboard.writeText(text).then(done, function () { b.textContent = "Select and copy"; });
    } catch (e) {
      b.textContent = "Select and copy";
    }
  });
});

// Docs pages: a copy button on every code block.
document.querySelectorAll(".doc pre").forEach(function (pre) {
  var code = pre.querySelector("code");
  if (!code) return;
  var b = document.createElement("button");
  b.type = "button";
  b.className = "copy";
  b.textContent = "Copy";
  b.addEventListener("click", function () {
    var done = function () { b.textContent = "Copied"; setTimeout(function () { b.textContent = "Copy"; }, 1600); };
    try {
      navigator.clipboard.writeText(code.textContent).then(done, function () { b.textContent = "Select and copy"; });
    } catch (e) {
      b.textContent = "Select and copy";
    }
  });
  // The button sits on a wrapper so a long line scrolls without running under it.
  var wrap = document.createElement("div");
  wrap.className = "codeblock";
  pre.parentNode.insertBefore(wrap, pre);
  wrap.appendChild(pre);
  wrap.appendChild(b);
});

// Docs pages: the page list starts folded on small screens.
var side = document.querySelector("details.side");
if (side && window.matchMedia("(max-width: 820px)").matches) side.open = false;
