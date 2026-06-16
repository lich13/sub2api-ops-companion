(function () {
  var prefetched = Object.create(null);

  function isSameDocumentNavigation(current, destination) {
    return (
      current.origin === destination.origin &&
      current.pathname === destination.pathname &&
      current.search === destination.search
    );
  }

  function linkFromEvent(event) {
    var link = event.target.closest ? event.target.closest("a[href]") : null;
    return link;
  }

  function isModifiedEvent(event) {
    return event.metaKey || event.ctrlKey || event.shiftKey || event.altKey;
  }

  function eligibleNavigation(link, event) {
    if (!link || link.hasAttribute("download") || link.getAttribute("target") === "_blank") {
      return null;
    }
    if (event && isModifiedEvent(event)) {
      return null;
    }
    var destination;
    try {
      destination = new URL(link.href, window.location.href);
    } catch (_) {
      return null;
    }
    var current = new URL(window.location.href);
    if (destination.origin !== current.origin || isSameDocumentNavigation(current, destination)) {
      return null;
    }
    return destination;
  }

  function prefetch(link, event) {
    var destination = eligibleNavigation(link, event);
    if (!destination) {
      return;
    }
    var key = destination.href;
    if (prefetched[key]) {
      return;
    }
    prefetched[key] = true;
    if (window.requestIdleCallback) {
      window.requestIdleCallback(function () {
        fetch(key, { credentials: "same-origin", headers: { "X-Sub2Ops-Prefetch": "1" } }).catch(function () {});
      }, { timeout: 1200 });
      return;
    }
    window.setTimeout(function () {
      fetch(key, { credentials: "same-origin", headers: { "X-Sub2Ops-Prefetch": "1" } }).catch(function () {});
    }, 80);
  }

  document.addEventListener("mouseover", function (event) {
    prefetch(linkFromEvent(event), event);
  });

  document.addEventListener("focusin", function (event) {
    prefetch(linkFromEvent(event), event);
  });

  document.addEventListener("touchstart", function (event) {
    prefetch(linkFromEvent(event), event);
  }, { passive: true });

  document.addEventListener("click", function (event) {
    if (event.defaultPrevented || event.button !== 0) {
      return;
    }
    var link = linkFromEvent(event);
    if (!eligibleNavigation(link, event)) {
      return;
    }
    document.body.classList.add("navigation-pending");
    link.classList.add("pending");
    link.setAttribute("aria-busy", "true");
  });
})();
