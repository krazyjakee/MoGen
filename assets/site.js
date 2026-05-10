// MoGen — gh-pages client script

(() => {
  'use strict';

  /* ---------------- syntax highlighting (.mog / shell) ---------------- */

  const MOG_KINDS = new Set([
    'scene', 'group', 'solid', 'material', 'module', 'use', 'instance',
    'box', 'plane', 'quad', 'cylinder', 'cone', 'sphere', 'capsule', 'torus',
    'prism', 'pyramid', 'disc', 'icosphere', 'rounded_box', 'ellipsoid',
    'superellipsoid', 'curved_plane', 'lathe', 'spline_tube', 'slab', 'post',
    'panel', 'wall', 'hemisphere', 'half_cylinder', 'torus_arc', 'frustum',
    'tube', 'wedge',
    'mirror', 'array', 'stack', 'grid',
    'union', 'difference', 'intersect',
    'connector', 'attach',
    'joint', 'clip', 'track', 'skin', 'bone',
    'spin', 'open_close', 'wave', 'flap', 'idle',
    'lod_scale',
  ]);

  const escapeHtml = (s) =>
    s.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;');

  function highlightMog(src) {
    let i = 0;
    const out = [];
    const n = src.length;
    let lastSig = '\n';

    while (i < n) {
      const c = src[i];

      if (c === '/' && src[i + 1] === '/') {
        let j = i;
        while (j < n && src[j] !== '\n') j++;
        out.push('<span class="tok-comment">' + escapeHtml(src.slice(i, j)) + '</span>');
        i = j;
        continue;
      }

      if (c === '"') {
        let j = i + 1;
        while (j < n && src[j] !== '"') {
          if (src[j] === '\\' && j + 1 < n) j++;
          j++;
        }
        if (j < n) j++;
        out.push('<span class="tok-string">' + escapeHtml(src.slice(i, j)) + '</span>');
        i = j;
        lastSig = '"';
        continue;
      }

      if (c === '$' && /[A-Za-z_]/.test(src[i + 1] || '')) {
        let j = i + 1;
        while (j < n && /[A-Za-z0-9_]/.test(src[j])) j++;
        out.push('<span class="tok-param">' + escapeHtml(src.slice(i, j)) + '</span>');
        i = j;
        lastSig = 'a';
        continue;
      }

      if (/[0-9]/.test(c) || (c === '-' && /[0-9.]/.test(src[i + 1] || ''))) {
        let j = i + 1;
        while (j < n && /[0-9.]/.test(src[j])) j++;
        out.push('<span class="tok-number">' + escapeHtml(src.slice(i, j)) + '</span>');
        i = j;
        lastSig = '0';
        continue;
      }

      if (/[A-Za-z_]/.test(c)) {
        let j = i + 1;
        while (j < n && /[A-Za-z0-9_]/.test(src[j])) j++;
        const word = src.slice(i, j);
        let k = j;
        while (k < n && (src[k] === ' ' || src[k] === '\t')) k++;
        const next = src[k];
        let cls = 'tok-ident';
        if (next === '=') {
          cls = 'tok-key';
        } else if (
          MOG_KINDS.has(word) &&
          (lastSig === '\n' || lastSig === '{' || lastSig === '}' || lastSig === ';' || lastSig === ',')
        ) {
          cls = 'tok-kind';
        }
        out.push('<span class="' + cls + '">' + escapeHtml(word) + '</span>');
        i = j;
        lastSig = 'a';
        continue;
      }

      if ('+-*/'.includes(c)) {
        out.push('<span class="tok-op">' + escapeHtml(c) + '</span>');
        i++;
        lastSig = c;
        continue;
      }

      if ('{}[](),;'.includes(c)) {
        out.push('<span class="tok-punct">' + escapeHtml(c) + '</span>');
        i++;
        lastSig = c;
        continue;
      }

      out.push(escapeHtml(c));
      if (c === '\n') lastSig = '\n';
      i++;
    }
    return out.join('');
  }

  function highlightShell(src) {
    const lines = src.split('\n');
    return lines.map((line) => {
      const comm = line.match(/^(\s*)#(.*)$/);
      if (comm) return `${escapeHtml(comm[1])}<span class="tok-comment">#${escapeHtml(comm[2])}</span>`;
      let html = escapeHtml(line);
      html = html.replace(/(&quot;[^&]*?&quot;|&#39;[^&]*?&#39;|&#x27;[^&]*?&#x27;)/g,
        (m) => `<span class="tok-string">${m}</span>`);
      html = html.replace(/(\s)(--?[A-Za-z][\w-]*)/g,
        (_, sp, f) => `${sp}<span class="tok-key">${f}</span>`);
      html = html.replace(/^(\s*)(mogen|cargo|git|sudo|brew|apt|apt-get|curl|wget|tar|unzip|cd|ls|cp|mv|mkdir|rustup|chmod|export|\.\/[\w./-]+)/,
        (_, sp, c) => `${sp}<span class="tok-kind">${c}</span>`);
      return html;
    }).join('\n');
  }

  function applyHighlighting() {
    document.querySelectorAll('pre code').forEach((el) => {
      if (el.dataset.hl === '1') return;
      // Look for the language on the <code>, then the parent <pre>.
      // Pandoc with --no-highlight emits `<pre class="mog"><code>…</code></pre>`,
      // hand-written blocks use `<pre><code class="language-mog">…</code></pre>`.
      const cls = el.className || '';
      const preCls = (el.parentElement && el.parentElement.className) || '';
      const m = cls.match(/language-(\w+)/) ||
                preCls.match(/language-(\w+)/) ||
                preCls.match(/^(?:.*\s)?(mog|mogen|sh|bash|shell|console)(?:\s.*)?$/);
      const lang = m ? m[1] : (el.dataset.lang || 'mog');
      const src = el.textContent;
      if (lang === 'mog' || lang === 'mogen') {
        el.innerHTML = highlightMog(src);
      } else if (lang === 'sh' || lang === 'bash' || lang === 'shell' || lang === 'console') {
        el.innerHTML = highlightShell(src);
      } else {
        return;
      }
      el.dataset.hl = '1';
      const pre = el.parentElement;
      if (pre && !pre.querySelector('.code-toolbar')) {
        const tag = document.createElement('span');
        tag.className = 'code-toolbar';
        tag.textContent = lang;
        pre.appendChild(tag);
      }
    });
  }

  /* ---------------- TOC scroll-spy ---------------- */

  function setupTocSpy() {
    const toc = document.querySelector('.toc');
    if (!toc) return;
    const links = Array.from(toc.querySelectorAll('a[href^="#"]'));
    if (!links.length) return;
    const map = new Map();
    links.forEach((a) => {
      const id = decodeURIComponent(a.getAttribute('href').slice(1));
      const el = document.getElementById(id);
      if (el) map.set(el, a);
    });
    const targets = Array.from(map.keys());
    if (!targets.length) return;

    const setActive = (el) => {
      links.forEach((a) => a.classList.remove('active'));
      const a = map.get(el);
      if (a) a.classList.add('active');
    };

    const observer = new IntersectionObserver(
      (entries) => {
        const visible = entries
          .filter((e) => e.isIntersecting)
          .sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
        if (visible.length) setActive(visible[0].target);
      },
      { rootMargin: '-72px 0px -70% 0px', threshold: 0 },
    );
    targets.forEach((t) => observer.observe(t));
  }

  /* ---------------- anchor links on headings ---------------- */

  function addHeadingAnchors() {
    document.querySelectorAll('.content h2[id], .content h3[id]').forEach((h) => {
      if (h.querySelector('.anchor')) return;
      const a = document.createElement('a');
      a.className = 'anchor';
      a.href = '#' + h.id;
      a.textContent = '#';
      h.appendChild(a);
    });
  }

  /* ---------------- nav active link ---------------- */

  function markActiveNav() {
    const path = (location.pathname.split('/').pop() || 'index.html').toLowerCase();
    document.querySelectorAll('.nav-links a').forEach((a) => {
      const href = (a.getAttribute('href') || '').toLowerCase();
      if (
        (path === '' && href === 'index.html') ||
        (href === path) ||
        (path === 'index.html' && href === 'index.html')
      ) {
        a.classList.add('active');
      }
    });
  }

  /* ---------------- GitHub releases fetcher (home page only) ---------------- */

  const REPO = 'krazyjakee/MoGen';

  function osFor(name) {
    const n = name.toLowerCase();
    if (n.includes('linux')) return 'linux';
    if (n.includes('darwin') || n.includes('macos') || n.endsWith('.dmg')) return 'macos';
    if (n.includes('windows') || n.endsWith('.msi') || n.endsWith('.zip')) return 'windows';
    return 'other';
  }

  function archFor(name) {
    const n = name.toLowerCase();
    if (n.includes('aarch64') || n.includes('arm64')) return 'arm64';
    if (n.includes('x86_64') || n.includes('amd64')) return 'x86_64';
    return '';
  }

  function fmtBytes(b) {
    if (!b && b !== 0) return '';
    const u = ['B', 'KB', 'MB', 'GB'];
    let i = 0;
    while (b >= 1024 && i < u.length - 1) { b /= 1024; i++; }
    return b.toFixed(b >= 10 || i === 0 ? 0 : 1) + ' ' + u[i];
  }

  const ICONS = {
    linux:   '<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 2c-2.7 0-4.5 2.6-4.5 5.4 0 1.4.4 2.5.9 3.4-1 .8-1.7 1.6-2 2.5-.5 1.5-.4 3 .2 4.4-.6.6-1 1.3-1 2 0 1.6 2.6 2.3 6.4 2.3s6.4-.7 6.4-2.3c0-.7-.4-1.4-1-2 .6-1.4.7-2.9.2-4.4-.3-.9-1-1.7-2-2.5.5-.9.9-2 .9-3.4C16.5 4.6 14.7 2 12 2z"/></svg>',
    macos:   '<svg viewBox="0 0 24 24" fill="currentColor"><path d="M16.5 12.3c0-2.6 2.1-3.8 2.2-3.9-1.2-1.7-3-2-3.7-2-1.6-.2-3.1 1-3.9 1-.8 0-2-.9-3.4-.9-1.7 0-3.4 1-4.3 2.6-1.8 3.2-.5 7.9 1.3 10.5.9 1.3 1.9 2.7 3.3 2.7 1.3-.1 1.8-.9 3.4-.9s2 .9 3.4.8c1.4 0 2.3-1.3 3.2-2.6.7-1 1.2-2.1 1.6-3.2-1.6-.6-3.1-2.4-3.1-4.1zM14 5.4c.7-.9 1.2-2.1 1.1-3.4-1 0-2.3.7-3 1.6-.7.8-1.3 2-1.1 3.2 1.2.1 2.4-.6 3-1.4z"/></svg>',
    windows: '<svg viewBox="0 0 24 24" fill="currentColor"><path d="M3 5.6 11 4.5v7.2H3V5.6zm0 7.6h8v7.2L3 19.4v-6.2zm9-8.8L22 3v9.7H12V4.4zm0 9.5h10V21l-10-1.5v-5.6z"/></svg>',
    other:   '<svg viewBox="0 0 24 24" fill="currentColor"><path d="M12 2 2 7v10l10 5 10-5V7L12 2zm0 2.3 7.5 3.7L12 11.7 4.5 8 12 4.3zM4 9.5l7 3.5v8L4 17.5v-8zm9 11.5v-8l7-3.5v8L13 21z"/></svg>',
  };

  const OS_LABEL = { linux: 'Linux', macos: 'macOS', windows: 'Windows', other: 'Other' };
  const OS_DESC = {
    linux:   'Tarball with both binaries, plus a <code>.deb</code> for Debian/Ubuntu.',
    macos:   '<code>.dmg</code> with MoGen Studio.app, plus a tarball.',
    windows: '<code>.msi</code> installer for MoGen Studio, plus a <code>.zip</code> for the CLI.',
    other:   'Source builds work everywhere — see the build-from-source instructions.',
  };

  function renderReleases(rel, container) {
    if (!rel || !rel.assets || !rel.assets.length) {
      container.innerHTML = `
        <div class="dl-empty">
          Couldn't reach the GitHub releases API — grab the latest build directly from
          <a href="https://github.com/${REPO}/releases/latest">github.com/${REPO}/releases/latest</a>,
          or build from source using the instructions below.
        </div>`;
      return;
    }

    const meta = document.getElementById('release-meta');
    if (meta) {
      const d = new Date(rel.published_at);
      meta.innerHTML = `
        <span class="pill">${escapeHtml(rel.tag_name)}</span>
        <span>Published ${d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' })}</span>
        <a href="${rel.html_url}" style="margin-left:auto">View on GitHub →</a>`;
    }

    const groups = { linux: [], macos: [], windows: [], other: [] };
    rel.assets.forEach((a) => {
      if (a.name === 'SHA256SUMS') return;
      groups[osFor(a.name)].push(a);
    });

    const order = ['linux', 'macos', 'windows', 'other'];
    container.innerHTML = order
      .filter((os) => groups[os].length)
      .map((os) => {
        const files = groups[os]
          .sort((a, b) => a.name.localeCompare(b.name))
          .map((a) => {
            const arch = archFor(a.name);
            const ext = (a.name.split('.').pop() || '').toLowerCase();
            const label = `${ext.toUpperCase()}${arch ? ' · ' + arch : ''}`;
            return `<a href="${a.browser_download_url}" rel="noopener">
                      <span>${label}</span>
                      <small>${fmtBytes(a.size)}</small>
                    </a>`;
          })
          .join('');
        return `
          <div class="dl-card">
            <div class="os">${ICONS[os]}<span>${OS_LABEL[os]}</span></div>
            <div class="desc">${OS_DESC[os]}</div>
            <div class="files">${files}</div>
          </div>`;
      })
      .join('');
  }

  function loadReleases() {
    const container = document.getElementById('downloads-grid');
    if (!container) return;
    fetch(`https://api.github.com/repos/${REPO}/releases/latest`)
      .then((r) => (r.ok ? r.json() : null))
      .then((rel) => renderReleases(rel, container))
      .catch(() => renderReleases(null, container));
  }

  /* ---------------- gallery hover-to-play ---------------- */

  function setupGallery() {
    document.querySelectorAll('.gallery-item').forEach((item) => {
      const v = item.querySelector('video');
      if (!v) return;
      const play = () => {
        // preload="none" defers loading until first play; subsequent hovers are instant.
        const p = v.play();
        if (p && typeof p.catch === 'function') p.catch(() => {});
      };
      const stop = () => {
        v.pause();
        try { v.currentTime = 0; } catch (_) {}
      };
      item.addEventListener('mouseenter', play);
      item.addEventListener('mouseleave', stop);
      // Touch devices have no hover — tap to play, tap again to stop.
      item.addEventListener('click', (e) => {
        if (!matchMedia('(hover: none)').matches) return;
        e.preventDefault();
        if (v.paused) play(); else stop();
      });
    });
  }

  /* ---------------- year stamp ---------------- */

  function stampYear() {
    document.querySelectorAll('[data-year]').forEach((el) => {
      el.textContent = new Date().getFullYear();
    });
  }

  /* ---------------- boot ---------------- */

  document.addEventListener('DOMContentLoaded', () => {
    applyHighlighting();
    addHeadingAnchors();
    setupTocSpy();
    markActiveNav();
    setupGallery();
    loadReleases();
    stampYear();
  });
})();
