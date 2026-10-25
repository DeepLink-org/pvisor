// Native previews use adjacent ports: Chinese N, English N + 1.
// Published sites retain the absolute language links rendered by the template.
(() => {
  const current = new URL(window.location.href);
  if (!["localhost", "127.0.0.1", "[::1]"].includes(current.hostname)) return;
  const offsets = { zh: 0, en: 1 };
  const locale = document.documentElement.lang;
  if (!current.port || !Object.hasOwn(offsets, locale)) return;

  function updateLanguageLinks() {
    document.querySelectorAll("a.md-select__link[hreflang]").forEach((link) => {
      const language = link.hreflang;
      if (!Object.hasOwn(offsets, language)) return;
      const port = Number(current.port) + offsets[language] - offsets[locale];
      if (port < 1 || port > 65535) return;
      const target = new URL(link.href);
      target.protocol = current.protocol;
      target.hostname = current.hostname;
      target.port = String(port);
      target.hash = window.location.hash;
      link.href = target.href;
    });
  }

  updateLanguageLinks();
  window.addEventListener("hashchange", updateLanguageLinks);
})();
