// The site's only script, shared by every page. It lives in a file, not inline, so the pages can be
// served with `script-src 'self'` and no 'unsafe-inline' (deploy/nginx-nightdrop*.conf): an
// injected inline script then does not run. Elements are built with the DOM API, never innerHTML,
// so `require-trusted-types-for 'script'` holds too.
(function () {
  'use strict';

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text) e.textContent = text;
    return e;
  }
  function setText(id, txt) {
    var e = document.getElementById(id);
    if (e && txt) e.textContent = String(txt);
  }

  // Apply config.js (window.NIGHTDROP_CONFIG), when it is present.
  var c = window.NIGHTDROP_CONFIG;
  if (c) {
    if (c.links && c.links.source) {
      var src = document.getElementById('source-link');
      if (src && /^https:\/\//i.test(c.links.source)) src.href = c.links.source;
    }
    // Only the home page has the hero text and takes its title from the config.
    if (c.app && document.getElementById('hero-headline')) {
      setText('hero-headline', c.app.headline);
      setText('hero-tagline', c.app.tagline);
      setText('donate-blurb', c.app.blurb);
      if (c.app.name) document.title = c.app.name + ' — private messaging with no account and no phone number';
    }
    var grid = document.getElementById('donate-grid');
    if (grid && Array.isArray(c.donations) && c.donations.length) {
      grid.textContent = '';
      c.donations.forEach(function (d) {
        var card = el('article', 'card coin');
        card.appendChild(el('h3', null, d.name + ' (' + d.ticker + ')'));
        card.appendChild(el('p', 'muted', d.note || ''));
        card.appendChild(el('code', 'addr', d.address));
        grid.appendChild(card);
      });
    }
    var box = document.getElementById('download-links');
    if (box && c.downloads) {
      // On the .onion mirror, serve the binaries from this same origin so a visitor never has to
      // leave Tor for a clearnet endpoint (GitHub). downloads_onion holds the /applications/ paths.
      var onion = /\.onion$/i.test(location.hostname);
      var links = (onion && c.downloads_onion) ? c.downloads_onion : c.downloads;
      var labels = { android: 'Android', windows: 'Windows', linux: 'Linux' };
      // Only an http(s) URL or a same-origin /applications/ path, never a javascript:/data: scheme,
      // even if the config were tampered with.
      var usable = function (u) {
        return typeof u === 'string' && (/^https?:\/\//i.test(u) || /^\/applications\/[\w.\/-]+$/.test(u));
      };
      box.textContent = '';
      // A platform with no download configured is left out, not shown as a dead button.
      Object.keys(labels).filter(function (k) { return usable(links[k]); }).forEach(function (k) {
        var a = el('a', 'btn ghost', labels[k]);
        a.href = links[k];
        // `download` only for a link to an actual file. Android points at a folder (onion) or the
        // release page (clearnet), where `download` would save the listing page instead.
        if (/\.[A-Za-z0-9]+$/.test(links[k])) a.setAttribute('download', '');
        box.appendChild(a);
      });
    }
  }

  // Frosted-glass sticky header: add .scrolled once the page leaves the top.
  var nav = document.querySelector('nav');
  if (nav) {
    var onScroll = function () { nav.classList.toggle('scrolled', window.scrollY > 8); };
    onScroll();
    window.addEventListener('scroll', onScroll, { passive: true });
  }
})();
