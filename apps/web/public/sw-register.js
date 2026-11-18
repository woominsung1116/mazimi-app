// Registers the service worker. A file, not an inline script, so the
// Content-Security-Policy needs no hash or nonce for it (next.config.ts).
(function () {
  if (!("serviceWorker" in navigator)) return;
  function register() {
    navigator.serviceWorker
      .register("/sw.js")
      .then(function (reg) { console.log("SW registered:", reg.scope); })
      .catch(function (err) { console.warn("SW registration failed:", err); });
  }
  // afterInteractive can run after the load event has already fired.
  if (document.readyState === "complete") register();
  else window.addEventListener("load", register);
})();
