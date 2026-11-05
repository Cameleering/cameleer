// Docs pages: copy buttons, the index filter and the "On this page" highlight.
(function () {
  // the navigation is always open on wide screens and collapsed on phones
  var navbox = document.querySelector('.navbox');
  var narrow = matchMedia('(max-width: 900px)');
  if (navbox) {
    var sync = function () { navbox.open = !narrow.matches; };
    sync();
    if (narrow.addEventListener) narrow.addEventListener('change', sync);
  }

  // copy buttons on code blocks
  document.querySelectorAll('.codeblock .copy').forEach(function (btn) {
    btn.addEventListener('click', function () {
      var code = btn.closest('.codeblock').querySelector('pre code');
      var done = function () { btn.textContent = 'Copied'; setTimeout(function () { btn.textContent = 'Copy'; }, 1400); };
      if (navigator.clipboard && window.isSecureContext) {
        navigator.clipboard.writeText(code.innerText).then(done);
      } else {
        var r = document.createRange(); r.selectNodeContents(code);
        var sel = getSelection(); sel.removeAllRanges(); sel.addRange(r);
        try { document.execCommand('copy'); done(); } catch (e) {}
        sel.removeAllRanges();
      }
    });
  });

  // filter box on the generated index pages (?q= pre-fills it)
  var input = document.getElementById('filter');
  if (input) {
    var table = document.querySelector('.index-table');
    var count = document.querySelector('.filter .count');
    var rows = table ? Array.prototype.slice.call(table.querySelectorAll('tbody tr:not(.cat)')) : [];
    var apply = function () {
      var words = input.value.toLowerCase().split(/\s+/).filter(Boolean), shown = 0;
      rows.forEach(function (tr) {
        var text = (tr.textContent + ' ' + (tr.getAttribute('data-cat') || '')).toLowerCase();
        var ok = words.every(function (w) { return text.indexOf(w) >= 0; });
        tr.hidden = !ok; if (ok) shown++;
      });
      table.querySelectorAll('tbody').forEach(function (tb) {
        tb.hidden = !tb.querySelector('tr:not(.cat):not([hidden])');
      });
      count.textContent = words.length ? shown + ' of ' + rows.length : rows.length + ' total';
    };
    var q = new URLSearchParams(location.search).get('q');
    if (q) input.value = q;
    input.addEventListener('input', apply);
    input.addEventListener('keydown', function (e) {
      if (e.key === 'Enter') { var first = rows.filter(function (r) { return !r.hidden; })[0]; if (first) location.href = first.querySelector('a').href; }
      if (e.key === 'Escape') { input.value = ''; apply(); }
    });
    apply();
  }

  // highlight the current section in "On this page"
  var toc = document.querySelector('.toc');
  if (toc && 'IntersectionObserver' in window) {
    var links = {};
    toc.querySelectorAll('a').forEach(function (a) { links[a.getAttribute('href').slice(1)] = a; });
    var current;
    var io = new IntersectionObserver(function (entries) {
      entries.forEach(function (e) {
        if (e.isIntersecting && links[e.target.id]) {
          if (current) current.classList.remove('on');
          current = links[e.target.id]; current.classList.add('on');
        }
      });
    }, { rootMargin: '-70px 0px -70% 0px' });
    Object.keys(links).forEach(function (id) { var h = document.getElementById(id); if (h) io.observe(h); });
  }

  // keep the current page visible in a long sidebar
  var cur = document.querySelector('.sidebar a[aria-current="page"]');
  if (cur && matchMedia('(min-width: 901px)').matches) {
    var sb = document.querySelector('.sidebar');
    if (cur.offsetTop > sb.clientHeight - 80) sb.scrollTop = cur.offsetTop - sb.clientHeight / 3;
  }
})();
