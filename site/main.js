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
