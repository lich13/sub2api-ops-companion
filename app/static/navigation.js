(function () {
  function isSameDocumentNavigation(current, destination) {
    return (
      current.origin === destination.origin &&
      current.pathname === destination.pathname &&
      current.search === destination.search
    );
  }

  document.addEventListener("click", function (event) {
    if (event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) {
      return;
    }
    var link = event.target.closest ? event.target.closest("a[href]") : null;
    if (!link || link.hasAttribute("download") || link.getAttribute("target") === "_blank") {
      return;
    }
    var destination;
    try {
      destination = new URL(link.href, window.location.href);
    } catch (_) {
      return;
    }
    var current = new URL(window.location.href);
    if (destination.origin !== current.origin || isSameDocumentNavigation(current, destination)) {
      return;
    }
    document.body.classList.add("navigation-pending");
    link.classList.add("pending");
    link.setAttribute("aria-busy", "true");
  });
})();
