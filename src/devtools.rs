//! What an agent can ask of the page: carries out the commands in
//! `commands.rs` and keeps the console and network logs they report on.
//!
//! The app's loop is single threaded and never blocks on Chrome, so a
//! command that needs an answer from Chrome parks its reply channel in
//! `pending` under the id of the DevTools request, and finishes when the
//! reply with that id comes past in `observe`.

use crate::control::{Output, Reply, Request};
use crate::engine::chrome::Chrome;
use crate::term::input::{Key, KeyCode};
use crate::ui;
use serde_json::{json, Value};
use std::collections::{HashMap, VecDeque};
use std::fmt::Write;
use std::time::{Duration, Instant};

/// Console messages kept, across pages.
const MAX_CONSOLE: usize = 2000;
/// Requests kept for the current page.
const MAX_REQUESTS: usize = 2000;
/// Longest text answer; more than this is cut, and says so.
const MAX_OUTPUT: usize = 30_000;
/// How long `open` and `reload` wait for the page to finish loading.
const LOAD_TIMEOUT: Duration = Duration::from_secs(15);
/// How long Chrome gets to answer a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long `back` and `forward` wait for the page to change.
const MOVE_TIMEOUT: Duration = Duration::from_secs(10);
/// A loaded page is reported on once nothing has been requested or logged
/// for this long, so that errors from its first fetches are counted.
const QUIET: Duration = Duration::from_millis(400);
/// But a page that never goes quiet is reported on after this long.
const QUIET_LIMIT: Duration = Duration::from_secs(4);
/// An action is answered once the page has been quiet this long after it,
/// so that what it set off (a fetch, a client-side route change, a dialog
/// taking focus) has happened and the errors it caused are counted.
const SETTLE: Duration = Duration::from_millis(250);
/// But a page that never goes quiet gets its answer after this long.
const SETTLE_LIMIT: Duration = Duration::from_millis(1500);
const DEFAULT_WAIT: f64 = 10.0;
/// What `style` prints when no property is named, as a JavaScript array.
const COMMON_STYLES: &str = "['display', 'position', 'width', 'height', 'margin', 'padding', 'border', \
     'left', 'top', 'z-index', 'visibility', 'opacity', 'color', 'background-color', 'font-family', 'font-size', \
     'font-weight', 'line-height', 'text-align']";

/// Helpers every page script can use, as `__kb`: finding elements by CSS
/// selector across shadow roots, by text (`text=…`), by number from `links`
/// (`@3`), or by focus (`:focus`); text that includes shadow content; and
/// hit testing that sees into shadow roots. Defined afresh with each script,
/// since the page may have been replaced since the last one.
const LIB: &str = r#"globalThis.__kb = {
  name: e => '<' + e.localName + (e.id ? '#' + e.id : '') +
    (e.classList && e.classList.length ? '.' + [...e.classList].join('.') : '') + '>',
  roots(root = document) {
    const out = [root];
    for (const e of root.querySelectorAll('*')) if (e.shadowRoot) out.push(...this.roots(e.shadowRoot));
    return out;
  },
  query(root, sel) {
    try { return [...root.querySelectorAll(sel)]; }
    catch (e) { throw new Error('not a valid CSS selector: ' + sel); }
  },
  all(sel) {
    if (sel === ':focus') { const el = this.focused(); return el ? [el] : []; }
    let m = /^@(\d+)$/.exec(sel);
    if (m) { const el = (globalThis.__kbLinks || [])[m[1] - 1]; return el && el.isConnected ? [el] : []; }
    m = /^text=([\s\S]+)$/.exec(sel);
    if (m) return this.byText(m[1]);
    if (sel.includes('>>>')) {
      let scope = [document];
      for (const part of sel.split('>>>')) scope = scope.flatMap(s => this.query(s.shadowRoot || s, part.trim()));
      return scope;
    }
    return this.roots().flatMap(r => this.query(r, sel));
  },
  find(sel) { return this.all(sel)[0] || null; },
  get(sel) {
    const el = this.find(sel);
    if (el) return el;
    if (/^@\d+$/.test(sel)) throw new Error('there is no ' + sel + ' on this page now; number the links with: kb ctl links');
    if (/^text=/.test(sel)) throw new Error('nothing visible on the page shows the text ' + JSON.stringify(sel.slice(5)));
    if (sel === ':focus') throw new Error('nothing has focus');
    if (/^[\w' ]+$/.test(sel) && sel.includes(' ')) throw new Error('no element matches ' + sel + '; to find one by its text, use text=' + sel);
    const frames = document.querySelectorAll('iframe').length;
    if (frames) throw new Error('no element matches ' + sel + ' (the page has ' + frames + (frames === 1 ? ' iframe' : ' iframes') +
      ', and kb does not look inside them; open the frame\'s own address to work in it)');
    throw new Error('no element matches ' + sel);
  },
  focused() {
    let a = document.activeElement;
    while (a && a.shadowRoot && a.shadowRoot.activeElement) a = a.shadowRoot.activeElement;
    return a && a !== document.body && a !== document.documentElement ? a : null;
  },
  shown(el) {
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && getComputedStyle(el).visibility === 'visible';
  },
  label(el) {
    const img = el.querySelector && el.querySelector('img[alt]');
    return (el.innerText || el.value || el.getAttribute('aria-label') || el.title || (img && img.alt) || '')
      .replace(/[\u200B-\u200D\uFEFF]/g, '').replace(/\s+/g, ' ').trim();
  },
  byText(want) {
    const w = want.replace(/\s+/g, ' ').trim().toLowerCase();
    const live = 'a[href], button, [role=button], [role=link], [role=tab], [role=menuitem], ' +
      'input[type=submit], input[type=button], summary, label';
    const controls = this.roots().flatMap(r => [...r.querySelectorAll(live)]).filter(e => this.shown(e));
    const exact = controls.find(e => this.label(e).toLowerCase() === w);
    const part = exact || controls.find(e => this.label(e).toLowerCase().includes(w));
    if (part) return [part];
    for (const r of this.roots()) {
      const walk = document.createTreeWalker(r, NodeFilter.SHOW_TEXT);
      for (let n; (n = walk.nextNode()); ) {
        const p = n.parentElement;
        if (p && n.data.toLowerCase().includes(w) && !p.closest('script, style, noscript') && this.shown(p)) return [p];
      }
    }
    // Text split across elements ("Sort by: <b>Best match</b>"): the innermost element holding all of it.
    return this.holding(w).slice(0, 1);
  },
  // Whether the element is drawn at all: not display: none, hidden or transparent.
  visible(el) {
    if (el.checkVisibility) return el.checkVisibility({visibilityProperty: true, opacityProperty: true});
    return this.shown(el);
  },
  text(el) {
    if (el.matches && el.matches('select')) return [...el.selectedOptions].map(o => o.label.trim()).join(', ');
    if (el.matches && el.matches('input, textarea')) return el.value;
    let t = el.innerText || '';
    for (const host of [el, ...el.querySelectorAll('*')].filter(h => h.shadowRoot)) {
      const inner = [...host.shadowRoot.children].map(c => this.text(c)).filter(s => s.trim()).join('\n');
      if (inner) t += (t ? '\n' : '') + inner;
    }
    return t;
  },
  at(x, y) {
    let hit = document.elementFromPoint(x, y);
    while (hit && hit.shadowRoot) {
      const inner = hit.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    return hit;
  },
  within(outer, inner) {
    for (let n = inner; n; n = n.parentNode || n.host) if (n === outer) return true;
    return false;
  },
  main() {
    const main = document.querySelector('main, [role=main]');
    if (main) return main;
    // One <article> is the page's content; several are a list of results or cards.
    const articles = document.querySelectorAll('article');
    return articles.length === 1 ? articles[0] : document.body;
  },
  // Innermost elements whose text (shadow content included) contains `w`,
  // lower case, in page order. Matches words split across elements too.
  holding(w) {
    const flat = e => (e.textContent || '').replace(/\s+/g, ' ').toLowerCase();
    const skip = 'script, style, noscript, template, head';
    const out = [];
    const descend = el => {
      if (el.shadowRoot) for (const c of el.shadowRoot.children) if (!c.matches(skip) && flat(c).includes(w)) descend(c);
      const kids = [...el.children].filter(c => !c.matches(skip) && flat(c).includes(w));
      if (kids.length) kids.forEach(descend);
      else if (flat(el).includes(w) && !out.some(o => this.within(el, o))) out.push(el);
    };
    const hosts = [document.body, ...this.roots().slice(1).map(r => r.host)];
    for (const h of hosts) {
      if (!h) continue;
      const tops = h === document.body ? [h] : [...h.shadowRoot.children];
      for (const t of tops) if (!t.matches(skip) && flat(t).includes(w)) descend(t);
    }
    return out.filter(e => this.shown(e));
  },
  // Where an element sits: inside a nav, sidebar, header or footer, if so.
  region(e) {
    const r = e.closest('nav, aside, header, footer, [role=navigation], [role=complementary], [role=banner], [role=contentinfo]');
    if (!r) return '';
    const role = r.getAttribute('role');
    return {navigation: 'nav', complementary: 'aside', banner: 'header', contentinfo: 'footer'}[role] || r.localName;
  },
};
"#;

const LINKS: &str = r#"((sel, limit, match) => {
  const regions = sel ? __kb.all(sel) : [__kb.main()];
  if (!regions.length) __kb.get(sel);
  const region = regions[0];
  const live = 'a[href], button, [role=button], [role=link], [role=menuitem], [role=menuitemradio], [role=menuitemcheckbox], ' +
    '[role=option], [role=tab], input[type=submit], input[type=button], summary';
  const found = [];
  for (const r of regions) {
    const inner = [r, ...r.querySelectorAll('*')].filter(e => e.shadowRoot).flatMap(h => __kb.roots(h.shadowRoot));
    const here = [...(r.matches && r.matches(live) ? [r] : []), ...[r, ...inner].flatMap(x => [...x.querySelectorAll(live)])];
    for (const e of here) if (!found.includes(e) && __kb.shown(e)) found.push(e);
  }
  // One entry per link: a search result's title, domain and snippet often all point the same way.
  const kept = [];
  for (const e of found) {
    const last = kept[kept.length - 1];
    if (last && e.matches('a[href]') && last.matches('a[href]') && last.href === e.href) {
      if (!__kb.label(last) && __kb.label(e)) kept[kept.length - 1] = e;
      continue;
    }
    kept.push(e);
  }
  found.length = 0;
  found.push(...kept);
  // Where a redirect wrapper (a search engine's click tracking) really goes.
  const target = href => {
    try {
      const u = new URL(href);
      const real = u.searchParams.get('uddg') || u.searchParams.get('url') || (u.pathname === '/url' && u.searchParams.get('q'));
      return real && /^https?:/.test(real) ? real + ' (via ' + u.hostname + ')' : href;
    } catch (e) { return href; }
  };
  globalThis.__kbLinks = found;
  const here = location.origin;
  const m = match ? match.toLowerCase() : '';
  const wanted = found.map((e, i) => [e, i]).filter(([e]) => !m || __kb.label(e).toLowerCase().includes(m)
    || (e.href || '').toLowerCase().includes(m));
  const lines = wanted.slice(0, limit).map(([e, i]) => {
    const text = __kb.label(e).slice(0, 80) || '(no text)';
    const where = __kb.region(e) ? ' (in ' + __kb.region(e) + ')' : '';
    if (!e.matches('a[href]')) {
      const role = e.getAttribute('role');
      return '@' + (i + 1) + ' [' + (role && role !== 'button' ? role : 'button') + '] ' + text + where;
    }
    const href = target(e.href.startsWith(here + '/') ? e.href.slice(here.length) : e.href);
    return '@' + (i + 1) + ' ' + text + ' -> ' + href + where;
  });
  const where = sel ? 'in ' + sel : region === document.body ? 'on the page'
    : 'in the main content, ' + __kb.name(region) + ' (for the whole page: kb ctl links body)';
  if (wanted.length > limit) lines.push('… ' + (wanted.length - limit) + ' more; narrow with a selector or --match, or raise --limit');
  if (region !== document.body && found.length < 5 && !sel)
    lines.push('(few links in the main content; for the whole page: kb ctl links body)');
  const matching = m ? ', ' + wanted.length + ' matching ' + JSON.stringify(match) + ' (numbered as in the full list)' : '';
  return [found.length + ' links and buttons ' + where + matching + '; click one with kb ctl click @N', ...lines].join('\n');
})"#;

const OUTLINE: &str = r#"(() => {
  const all = __kb.roots().flatMap(r => [...r.querySelectorAll('h1, h2, h3, h4, h5, h6')]).filter(h => __kb.shown(h));
  const lines = ['title: ' + document.title];
  const main = __kb.main();
  if (main !== document.body) lines.push('main content: ' + __kb.name(main) + '; headings outside it are marked');
  const outside = h => main !== document.body && !__kb.within(main, h) ? ' (in ' + (__kb.region(h) || 'page') + ')' : '';
  for (const h of all.slice(0, 150)) lines.push('  '.repeat(+h.localName[1] - 1) + h.localName + ' ' + __kb.label(h).slice(0, 100) + outside(h));
  if (all.length > 150) lines.push('… ' + (all.length - 150) + ' more headings');
  if (!all.length) lines.push('(the page has no visible headings)');
  return lines.join('\n');
})()"#;

const SECTION: &str = r#"((scope, want) => {
  const w = want.replace(/\s+/g, ' ').trim().toLowerCase();
  const tags = 'h1, h2, h3, h4, h5, h6, [role=heading]';
  const within = scope ? [__kb.get(scope)] : __kb.roots();
  const heads = within.flatMap(r => [...r.querySelectorAll(tags)]).filter(h => __kb.shown(h));
  const head = heads.find(h => __kb.label(h).toLowerCase() === w) || heads.find(h => __kb.label(h).toLowerCase().includes(w));
  if (!head) throw new Error('no heading reads ' + JSON.stringify(want) + '; see the headings with: kb ctl outline');
  // Show the user the part being read.
  head.scrollIntoView({block: 'start', behavior: 'instant'});
  const levelOf = h => +(/^h(\d)$/.exec(h.localName) || [])[1] || +h.getAttribute('aria-level') || 6;
  const level = levelOf(head);
  // Up past wrappers that hold little but the heading (Wikipedia's [edit] links).
  let node = head;
  while (node.parentElement && node.parentElement !== document.body
         && __kb.text(node.parentElement).trim().length <= __kb.label(head).length + 20) node = node.parentElement;
  const parts = [];
  for (let s = node.nextElementSibling; s; s = s.nextElementSibling) {
    const h = s.matches(tags) ? s : s.querySelector(tags);
    if (h && levelOf(h) <= level) break;
    const t = __kb.text(s).trim();
    if (t) parts.push(t);
  }
  // Nothing after it: the heading tops a card, whose box holds the content.
  const body = parts.length ? parts.join('\n') : node.parentElement ? __kb.text(node.parentElement).trim() : '';
  return '[' + __kb.label(head) + ']\n' + (body || '(nothing under this heading)');
})"#;

const STORAGE: &str = r#"(() => {
  const show = (name, store) => {
    let keys = [];
    try { keys = Object.keys(store); } catch (e) { return [name + ': not available here']; }
    if (!keys.length) return [name + ': empty'];
    return [name + ': ' + keys.length + (keys.length === 1 ? ' key' : ' keys'),
      ...keys.slice(0, 40).map(k => '  ' + k + ' = ' + String(store.getItem(k)).slice(0, 120))];
  };
  const cookies = document.cookie ? document.cookie.split('; ').map(c => c.split('=')[0]) : [];
  return [...show('localStorage', localStorage), ...show('sessionStorage', sessionStorage),
    'cookies readable by the page: ' + (cookies.length ? cookies.join(', ') : 'none')].join('\n');
})()"#;

const TABLES: &str = r#"((index, limit) => {
  const tables = __kb.roots().flatMap(r => [...r.querySelectorAll('table')]).filter(t => __kb.shown(t) && t.rows.length > 1);
  if (!tables.length) return 'the page has no visible tables';
  const cells = row => [...row.cells].map(c => __kb.text(c).replace(/\s+/g, ' ').trim());
  if (index === null) {
    const lines = [tables.length + ' tables; read one with kb ctl tables N'];
    tables.forEach((t, i) => {
      let title = t.caption ? __kb.label(t.caption) : '';
      // The nearest heading before the table, looking back through earlier siblings and up.
      for (let e = t, steps = 0; !title && e && e !== document.body && steps < 60; steps++) {
        const prev = e.previousElementSibling;
        if (!prev) { e = e.parentElement; continue; }
        e = prev;
        const inside = e.querySelectorAll('h1, h2, h3, h4, h5, h6');
        const h = e.matches('h1, h2, h3, h4, h5, h6') ? e : inside[inside.length - 1];
        if (h) title = __kb.label(h);
      }
      const head = cells(t.rows[0]).join(' | ').slice(0, 120);
      lines.push((i + 1) + '. ' + t.rows.length + ' rows x ' + t.rows[0].cells.length + ' columns' +
        (title ? ', under "' + title.slice(0, 60) + '"' : '') + ': ' + head);
    });
    return lines.join('\n');
  }
  const t = tables[index - 1];
  if (!t) throw new Error('there are ' + tables.length + ' tables; give a number from 1 to ' + tables.length);
  t.scrollIntoView({block: 'start', behavior: 'instant'});
  const rows = [...t.rows].slice(0, limit).map(r => cells(r).join('\t'));
  if (t.rows.length > limit) rows.push('… ' + (t.rows.length - limit) + ' more rows; raise --limit');
  return ['table ' + index + ': ' + t.rows.length + ' rows, tab separated', ...rows].join('\n');
})"#;

const IMAGES: &str = r#"((sel, limit) => {
  const region = sel ? __kb.get(sel) : __kb.main();
  const all = [region, ...__kb.roots().slice(1).map(r => r.host).filter(h => h && __kb.within(region, h)).map(h => h.shadowRoot)]
    .flatMap(r => [...r.querySelectorAll('img, svg[role=img], picture img')])
    .filter((e, i, list) => list.indexOf(e) === i && __kb.shown(e) && e.getBoundingClientRect().width >= 24);
  if (!all.length) return 'no visible images' + (sel ? ' in ' + sel : ' in the main content (try: kb ctl images body)');
  const lines = [all.length + ' images' + (sel ? ' in ' + sel : region === document.body ? '' : ' in the main content') +
    '; look at one with kb ctl screenshot --selector'];
  all.slice(0, limit).forEach((e, i) => {
    const r = e.getBoundingClientRect();
    const figure = e.closest('figure');
    const caption = figure && figure.querySelector('figcaption') ? __kb.label(figure.querySelector('figcaption')) : '';
    const link = e.closest('a[href]');
    lines.push((i + 1) + '. ' + (e.alt ? 'alt "' + e.alt.slice(0, 100) + '"' : 'no alt text') +
      ', shown ' + Math.round(r.width) + 'x' + Math.round(r.height) +
      (e.naturalWidth ? ', file ' + e.naturalWidth + 'x' + e.naturalHeight : '') +
      (caption ? '\n   caption: ' + caption.slice(0, 160) : '') +
      '\n   src: ' + (e.currentSrc || e.src || '(inline svg)') + (link ? '\n   links to: ' + link.href : ''));
  });
  if (all.length > limit) lines.push('… ' + (all.length - limit) + ' more');
  return lines.join('\n');
})"#;

const FIND: &str = r#"((want, limit) => {
  const w = want.toLowerCase();
  const heads = __kb.roots().flatMap(r => [...r.querySelectorAll('h1, h2, h3, h4, h5, h6')]).filter(h => __kb.shown(h));
  const top = n => { while (n.getRootNode() !== document && n.getRootNode().host) n = n.getRootNode().host; return n; };
  // In document order, counting a heading in a shadow root at its host.
  heads.sort((a, b) => top(a) === top(b) ? 0 : top(a).compareDocumentPosition(top(b)) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1);
  const blockOf = n => {
    let e = n.parentElement;
    while (e && e.parentElement && getComputedStyle(e).display.startsWith('inline')) e = e.parentElement;
    return e;
  };
  const blocks = [];
  for (const el of __kb.holding(w)) {
    const block = blockOf({parentElement: el});
    if (block && !blocks.includes(block)) blocks.push(block);
  }
  blocks.sort((a, b) => top(a) === top(b) ? 0 : top(a).compareDocumentPosition(top(b)) & Node.DOCUMENT_POSITION_FOLLOWING ? -1 : 1);
  if (!blocks.length) return 'no visible text matches ' + JSON.stringify(want) + ' (text in images, canvases and iframes is not searched)';
  blocks[0].scrollIntoView({block: 'center', behavior: 'instant'});
  const lines = blocks.slice(0, limit).map((block, i) => {
    // A label alone ("Latest", "Downloads") says little: widen to its row or card.
    let ctx = block;
    for (let i = 0; i < 3 && ctx.parentElement && (ctx.innerText || '').trim().length < 60
         && (ctx.parentElement.innerText || '').length < 400; i++) ctx = ctx.parentElement;
    const text = (__kb.text(ctx) || ctx.textContent).replace(/\s+/g, ' ').trim();
    const at = Math.max(0, text.toLowerCase().indexOf(w));
    const from = Math.max(0, at - 120), to = Math.min(text.length, at + w.length + 120);
    const context = (from > 0 ? '…' : '') + text.slice(from, to) + (to < text.length ? '…' : '');
    let heading = '';
    for (const h of heads) {
      if (h === block || h.contains(block) || block.contains(h)) { heading = __kb.label(h); continue; }
      if (top(h) !== top(block) && top(h).compareDocumentPosition(top(block)) & Node.DOCUMENT_POSITION_FOLLOWING) heading = __kb.label(h);
      else if (top(h) !== top(block)) break;
    }
    return (i + 1) + '. ' + (heading ? '[' + heading.slice(0, 60) + '] ' : '') + context;
  });
  const shown = blocks.length > limit ? ', the first ' + limit + ' shown' : '';
  return [blocks.length + ' places show ' + JSON.stringify(want) + shown + '; the window is scrolled to the first', ...lines].join('\n');
})"#;

/// Script fragment: how an element is named in answers, `<div#id>`.
const NAME_OF: &str = "(e => '<' + e.localName + (e.id ? '#' + e.id : '') + \
     (e.classList.length ? '.' + [...e.classList].join('.') : '') + '>')";

/// For `locate`, before a click: listen for it, to tell afterwards whether
/// it reached the page.
const HEAR: &str = "globalThis.__kbHeard = false; \
     addEventListener('mousedown', () => { globalThis.__kbHeard = true; }, {capture: true, once: true});";

/// For `locate`, before a click: refuse if the click would land elsewhere.
const CLICKABLE: &str = "{ const b = el.getBoundingClientRect(); \
     const hit = b.width > 0 && b.height > 0 ? __kb.at(b.left + b.width / 2, b.top + b.height / 2) : el; \
     if (!hit) throw new Error('the element is off screen, at ' + Math.round(b.left) + ',' + Math.round(b.top) + \
       ' in the window, and can\\'t be scrolled into view, so it can\\'t be clicked; see: kb ctl box'); \
     const labels = el.labels ? [...el.labels] : []; \
     if (hit !== el && !__kb.within(el, hit) && !__kb.within(hit, el) && !labels.some(label => __kb.within(label, hit))) \
       throw new Error('the element is covered by ' + NAME(hit) + ', which would get the click instead; see: kb ctl box'); \
     if (hit !== el && __kb.within(el, hit)) landed = NAME(hit); }";

/// Where an element is on the page, without moving the page, for a screenshot.
const SHOT_RECT: &str = "(sel => { const el = __kb.get(sel); \
     const r = el.getBoundingClientRect(), x = r.left + scrollX, y = r.top + scrollY; \
     if (r.width <= 0 || r.height <= 0) throw new Error('the element has no size, so there is nothing to capture'); \
     if (x + r.width <= 0 || y + r.height <= 0) throw new Error('the element is outside the page, at ' + \
       Math.round(x) + ',' + Math.round(y) + ', so there is nothing to capture; see: kb ctl box'); \
     return {x, y, width: r.width, height: r.height}; })";

const BOX: &str = "(sel => { const el = __kb.get(sel); \
     const r = el.getBoundingClientRect(), style = getComputedStyle(el), n = Math.round; \
     const hidden = style.display === 'none' ? 'display: none' \
       : style.visibility !== 'visible' ? 'visibility: ' + style.visibility \
       : +style.opacity === 0 ? 'opacity: 0' \
       : r.width === 0 || r.height === 0 ? 'it has no size' : ''; \
     const x = r.left + scrollX, y = r.top + scrollY; \
     const outside = x + r.width <= 0 || y + r.height <= 0 ? 'outside the page, so it can\\'t be scrolled to' \
       : r.bottom <= 0 ? 'above the window' : r.top >= innerHeight ? 'below the window' \
       : r.right <= 0 ? 'left of the window' : r.left >= innerWidth ? 'right of the window' : ''; \
     const lines = ['size: ' + n(r.width) + 'x' + n(r.height), \
       'position: ' + n(x) + ',' + n(y) + ' on the page; ' + n(r.left) + ',' + n(r.top) + ' in the window', \
       'rendered: ' + (hidden ? 'no (' + hidden + ')' : 'yes'), \
       'in the window: ' + (outside ? 'no (' + outside + ')' : 'yes')]; \
     if (!hidden && !outside) { \
       const hit = __kb.at(r.left + r.width / 2, r.top + r.height / 2); \
       const covered = hit && hit !== el && !__kb.within(el, hit) && !__kb.within(hit, el); \
       lines.push('covered: ' + (covered ? 'yes, by ' + NAME(hit) + ' at its centre' : 'no')); } \
     return lines.join('\\n'); })";

const SCROLL: &str = "(async to => { const before = document.documentElement.scrollHeight; \
     if (to === 'top') scrollTo(0, 0); \
     else if (to === 'bottom') scrollTo(0, document.documentElement.scrollHeight); \
     else if (/^\\d+$/.test(to)) scrollTo(0, +to); \
     else { const el = __kb.find(to); \
       if (!el) throw new Error('no element matches ' + to + '; scroll takes top, bottom, a number of pixels, or a selector'); \
       el.scrollIntoView({block: 'center', inline: 'center', behavior: 'instant'}); } \
     for (let i = 0; i < 10 && document.documentElement.scrollHeight === before; i++) await new Promise(r => setTimeout(r, 100)); \
     const after = document.documentElement.scrollHeight, most = Math.max(0, after - innerHeight); \
     return 'scrolled to ' + Math.min(most, Math.round(scrollY)) + ' of ' + most + ' pixels' + \
       (after > before ? '; the page grew from ' + before + ' to ' + after + ' pixels as it loaded more' : ''); })";

const SELECT: &str = "((sel, want) => { const el = __kb.get(sel); \
     if (el.localName !== 'select') throw new Error('that element is a <' + el.localName + '>, not a <select>'); \
     const options = [...el.options], label = o => o.label.trim(); \
     const option = options.find(o => o.value === want) || options.find(o => label(o) === want) \
       || options.find(o => label(o).toLowerCase() === want.toLowerCase()); \
     if (!option) throw new Error('there is no option ' + JSON.stringify(want) + '; the options are: ' + options.map(label).join(', ')); \
     el.value = option.value; \
     el.dispatchEvent(new Event('input', {bubbles: true})); \
     el.dispatchEvent(new Event('change', {bubbles: true})); \
     return 'selected ' + label(option) + ' in ' + sel; })";

/// How `eval` turns an object into something printable: elements, and
/// lists of them, by name.
const CONTENTS: &str = "function() { \
     const show = v => v instanceof Element ? NAME(v) : v; \
     if (this instanceof NodeList || this instanceof HTMLCollection || Array.isArray(this)) return [...this].map(show); \
     return this; }";

/// `CLICKABLE`, testing the point `at` in the element rather than its middle.
fn clickable(at: Option<(f64, f64)>) -> String {
    let template = names(CLICKABLE);
    match at {
        Some((x, y)) => template
            .replace("b.left + b.width / 2", &format!("b.left + {x}"))
            .replace("b.top + b.height / 2", &format!("b.top + {y}")),
        None => template,
    }
}

/// A template above with `NAME` filled in.
fn names(template: &str) -> String {
    template.replace("NAME", NAME_OF)
}

/// A script from a template above, with the `__kb` helpers in front.
fn script(template: &str) -> String {
    format!("{LIB}\n{}", names(template))
}
const MAX_WAIT: f64 = 60.0;

struct Message {
    /// Position in the stream of all messages, to tell pages apart.
    seq: u64,
    level: String,
    text: String,
    /// Where it came from, as `file:line`, when known.
    source: String,
    /// The full address it came from, for `check --ignore`.
    url: String,
}

struct NetRequest {
    id: String,
    method: String,
    url: String,
    kind: String,
    status: Option<u64>,
    /// Why it failed, if it did.
    error: Option<String>,
    finished: bool,
    cached: bool,
    /// The document load it belongs to.
    loader: String,
    /// Whether it is the page itself.
    page: bool,
}

impl Message {
    fn line(&self) -> String {
        match (self.level.as_str(), self.source.as_str()) {
            (PAGE, _) => format!("--- page: {} ---", self.text),
            (level, "") => format!("[{level}] {}", self.text),
            (level, source) => format!("[{level}] {} ({source})", self.text),
        }
    }
}

/// The level of the marker logged where a new page begins.
const PAGE: &str = "page";
/// The level of the record of a dialog and how it was answered.
const DIALOG: &str = "dialog";

impl NetRequest {
    fn line(&self) -> String {
        let state = match (&self.error, self.status) {
            (Some(error), _) => format!("FAILED ({error})"),
            (None, Some(status)) if self.cached => format!("{status} (cached)"),
            (None, Some(status)) if status >= 400 && is_favicon(&self.url) => format!("{status} (not counted)"),
            (None, Some(status)) => status.to_string(),
            (None, None) if self.finished => "done".to_owned(),
            (None, None) => "pending".to_owned(),
        };
        format!("{state} {} {} ({})", self.method, self.url, self.kind)
    }

    fn failed(&self) -> bool {
        !is_favicon(&self.url) && (self.error.is_some() || self.status.is_some_and(|status| status >= 400))
    }
}

/// What to do with an element once its position is known.
enum Then {
    /// The selector, 1 for a click or 2 for a double click, and where in
    /// the element (CSS pixels from its top left) if not the middle.
    Click(String, u32, Option<(f64, f64)>),
    Hover(String),
    Type(String, String),
    Screenshot(ShotSize),
}

/// How big a screenshot comes out.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ShotSize {
    /// Image pixels per CSS pixel: 0.5, 1, or the device pixel ratio.
    Factor(f64),
    Sharp,
    /// An image this many pixels wide.
    Width(f64),
}

/// The tallest screenshot kb makes; taller ones are scaled down to fit.
const MAX_SHOT_HEIGHT: f64 = 8000.0;

impl ShotSize {
    fn parse(text: Option<&str>) -> Result<ShotSize, String> {
        match text.unwrap_or("normal") {
            "normal" => Ok(ShotSize::Factor(1.0)),
            "small" => Ok(ShotSize::Factor(0.5)),
            "sharp" => Ok(ShotSize::Sharp),
            width => match width.trim_end_matches("px").parse::<f64>() {
                Ok(width) if (16.0..=10_000.0).contains(&width) => Ok(ShotSize::Width(width)),
                _ => {
                    Err(format!("{width:?} isn't a size; give small, normal, sharp, or a width in pixels such as 800"))
                }
            },
        }
    }

    /// Image pixels per CSS pixel for a region `width` CSS pixels wide.
    fn factor(self, width: f64, dpr: f64) -> f64 {
        match self {
            ShotSize::Factor(factor) => factor,
            ShotSize::Sharp => dpr,
            // Never sharper than the screen: that would only invent pixels.
            ShotSize::Width(pixels) => (pixels / width.max(1.0)).min(dpr),
        }
    }
}

/// How things stood before an action, to tell what it caused.
#[derive(Clone)]
struct Baseline {
    seq: u64,
    failed: usize,
    url: String,
    /// How many page loads had started.
    loads: u64,
    /// The document then, to tell a new page from a move within this one.
    loader: String,
}

/// A command waiting for the page to finish loading.
struct LoadWait {
    reply: Reply,
    /// What to say before the report on the page, if anything.
    said: String,
    deadline: Instant,
    /// When loading finished. The answer then waits for the page to go quiet.
    stopped: Option<Instant>,
    /// Whether this is `check`, for which an unhealthy page is a failure.
    check: bool,
    /// For `check`: hosts whose errors and failed requests don't count.
    ignore: Vec<String>,
}

/// An action that is done but not yet answered: errors it causes take a
/// moment to arrive.
struct Settling {
    reply: Reply,
    message: String,
    since: Baseline,
    at: Instant,
    /// For a click: the element, to check the page heard the click (unless
    /// it moved on) and to click it by script if not.
    verify: Option<String>,
}

/// A command waiting on a reply from Chrome.
enum Pending {
    /// Answer with the value the expression produced.
    Eval(Reply),
    /// An object's contents are coming, as JSON; if they can't be had,
    /// answer with this description of it.
    Contents(Reply, String),
    Navigate(Reply),
    /// The element's rectangle is coming; then act on it.
    Rect(Reply, Then),
    /// The page's layout is coming; then capture the window, or with `true`
    /// the whole page.
    PageSize(Reply, ShotSize, bool),
    Image(Reply),
    /// The history list is coming; then step this far through it.
    History(Reply, i64),
    /// Once Chrome confirms the action, answer with this text and whatever
    /// went wrong on the page since the baseline; and if set, first check
    /// the page heard the click.
    Done(Reply, String, Baseline, Option<String>),
    /// For `style --why`: an element by reference, then its DOM node, then
    /// the rules that match it, then its computed value.
    StyleOf(Reply, String, String),
    StyleNode(Reply, String, String),
    StyleRules(Reply, String, String, u64),
    StyleValue(Reply, String, String, String),
    /// Whether the page heard a click is coming.
    Heard(Settling),
    /// A click by script is done; answer once the page settles.
    Clicked(Reply, String, Baseline),
    /// An element is coming by reference, to list its event listeners.
    ListenersOf(Reply, String),
    /// Its event listeners are coming.
    Listeners(Reply, String),
    /// The page's title is coming; answer with it between these two parts.
    Status(Reply, String, String),
    /// A script that acts on the page is running; answer with what it
    /// returns and whatever went wrong since the baseline.
    Acted(Reply, Baseline),
    /// One check of a `wait`; the number identifies the wait.
    WaitCheck(u64),
}

/// A `wait` in progress, checked on every tick so that it survives the page
/// being replaced.
struct Wait {
    id: u64,
    /// What is being waited for, in words, and the script that tests it.
    what: String,
    check: String,
    deadline: Instant,
    timeout: f64,
    checking: bool,
    reply: Reply,
}

/// What the app knows about the page that the commands report.
pub struct Page<'a> {
    pub url: &'a str,
    pub loading: bool,
    /// Viewport in CSS pixels.
    pub viewport: (u32, u32),
    /// Device pixels per CSS pixel in Chrome's images.
    pub scale: f64,
    /// Browser zoom, 1 for 100%.
    pub zoom: f64,
}

#[derive(Default)]
pub struct Devtools {
    console: VecDeque<Message>,
    next_seq: u64,
    /// First message of the current page.
    page_seq: u64,
    requests: Vec<NetRequest>,
    /// The document load that started the current page.
    loader: String,
    url: String,
    pending: HashMap<u64, (Pending, Instant)>,
    /// Commands waiting for the page to finish loading.
    loading: Vec<LoadWait>,
    /// Page loads started, and whether one is under way.
    loads: u64,
    busy: bool,
    /// When the page last requested or logged something.
    active: Option<Instant>,
    /// Why the page's own document last failed to load, if it did.
    document_error: Option<String>,
    /// `back` and `forward` waiting for the page to change.
    moving: Vec<(Reply, Instant)>,
    waits: Vec<Wait>,
    next_wait: u64,
    settling: Vec<Settling>,
    /// Device pixels per CSS pixel, as of the last screenshot command.
    dpr: f64,
    /// The network speed being imitated, if any.
    throttle: String,
    /// Whether the DOM and CSS domains are on, for `style --why`.
    css_on: bool,
    /// Stylesheets by id: where each came from and the line it starts at.
    sheets: HashMap<String, (String, u64, bool)>,
    /// How dialogs are answered: dismissed if set, else accepted, and with
    /// this text typed into prompts if given.
    dismiss: bool,
    prompt_text: Option<String>,
}

fn text(reply: &Reply, text: impl Into<String>) {
    let mut text: String = text.into();
    if text.len() > MAX_OUTPUT {
        let cut = (0..=MAX_OUTPUT).rev().find(|at| text.is_char_boundary(*at)).unwrap_or(0);
        let more = text[cut..].chars().count();
        text.truncate(cut);
        let _ = write!(text, "\n… ({more} more characters not shown)");
    }
    let _ = reply.send(Ok(Output::Text(text)));
}

fn fail(reply: &Reply, error: impl Into<String>) {
    let _ = reply.send(Err(error.into()));
}

/// A JavaScript string literal.
fn quote(text: &str) -> String {
    Value::String(text.to_owned()).to_string()
}

fn plural(count: usize, noun: &str) -> String {
    match count {
        0 => format!("no {noun}s"),
        1 => format!("1 {noun}"),
        _ => format!("{count} {noun}s"),
    }
}

/// The outcome of a `Runtime.evaluate`: the value, or what was thrown.
fn evaluated(result: Result<&Value, String>) -> Result<&Value, String> {
    let result = result?;
    if let Some(details) = result.get("exceptionDetails") {
        let thrown = details["exception"]["description"].as_str().or(details["text"].as_str());
        // The first line is the message; the rest is a stack through our own wrapper.
        let line = thrown.unwrap_or("the script threw").lines().next().unwrap_or("");
        // Our own scripts throw plain Errors; "TypeError:" and such stay.
        return Err(line.strip_prefix("Error: ").unwrap_or(line).to_owned());
    }
    Ok(&result["result"])
}

/// A value from the page as text: strings bare, everything else as JSON.
fn show(object: &Value) -> String {
    match object.get("value") {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => preview(object)
            .or(object["description"].as_str().map(str::to_owned))
            .unwrap_or_else(|| object["type"].as_str().unwrap_or("undefined").to_owned()),
    }
}

/// An object or array logged to the console, from the summary of its
/// contents that Chrome attaches: `{a: 1, b: "x"}`, `[1, 2, …]`.
fn preview(object: &Value) -> Option<String> {
    let preview = &object["preview"];
    let array = preview["subtype"] == "array";
    let mut parts: Vec<String> = preview["properties"]
        .as_array()?
        .iter()
        .map(|property| {
            let value = match (property["type"].as_str(), property["value"].as_str().unwrap_or("")) {
                (Some("string"), value) => quote(value),
                (_, value) => value.to_owned(),
            };
            match property["name"].as_str() {
                Some(name) if !array => format!("{name}: {value}"),
                _ => value,
            }
        })
        .collect();
    if preview["overflow"] == true {
        parts.push("…".to_owned());
    }
    Some(if array { format!("[{}]", parts.join(", ")) } else { format!("{{{}}}", parts.join(", ")) })
}

/// `https://site/app.js?v=2` and line 12 as `app.js:12`.
fn source(url: &str, line: Option<u64>) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let name = path.rsplit('/').find(|part| !part.is_empty()).unwrap_or("");
    match line {
        Some(line) if !name.is_empty() => format!("{name}:{}", line + 1),
        _ => name.to_owned(),
    }
}

/// `Ctrl+a`, `Shift+Tab`, `Enter`, `x`.
fn parse_key(name: &str) -> Option<Key> {
    let mut key = Key { code: KeyCode::Esc, ctrl: false, alt: false, shift: false, cmd: false };
    let mut parts = name.split('+').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_some() {
            match part.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => key.ctrl = true,
                "alt" | "option" => key.alt = true,
                "shift" => key.shift = true,
                _ => return None,
            }
            continue;
        }
        key.code = match part.to_ascii_lowercase().as_str() {
            "enter" | "return" => KeyCode::Enter,
            "tab" => KeyCode::Tab,
            "escape" | "esc" => KeyCode::Esc,
            "backspace" => KeyCode::Backspace,
            "delete" => KeyCode::Delete,
            "arrowleft" | "left" => KeyCode::Left,
            "arrowright" | "right" => KeyCode::Right,
            "arrowup" | "up" => KeyCode::Up,
            "arrowdown" | "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" => KeyCode::PageUp,
            "pagedown" => KeyCode::PageDown,
            "space" => KeyCode::Char(' '),
            name if name.len() > 1
                && name.starts_with('f')
                && name[1..].parse::<u8>().is_ok_and(|n| (1..=12).contains(&n)) =>
            {
                KeyCode::F(name[1..].parse().unwrap_or(1))
            }
            _ => {
                let mut chars = part.chars();
                match (chars.next(), chars.next()) {
                    (Some(c), None) => KeyCode::Char(c),
                    _ => return None,
                }
            }
        };
    }
    Some(key)
}

/// Script that finds an element, brings it into view, and returns where it
/// is in the viewport.
fn locate(selector: &str, prepare: &str) -> String {
    format!(
        "{LIB}\n(() => {{ const el = __kb.get({sel}); \
         el.scrollIntoView({{block: 'center', inline: 'center', behavior: 'instant'}}); \
         let landed = ''; {prepare} const r = el.getBoundingClientRect(); \
         return {{x: r.x, y: r.y, width: r.width, height: r.height, landed, target: {sel}}}; }})()",
        sel = quote(selector),
    )
}

/// For `locate`: focus a text field, refusing anything that isn't one.
const FOCUS: &str =
    "if (el.localName === 'select') throw new Error('that element is a <select>; use: kb ctl select'); \
     if (el.matches('input:is([type=checkbox], [type=radio])')) \
       throw new Error('that element is a ' + el.type + '; use: kb ctl click'); \
     if (!el.isContentEditable && !el.matches('textarea, input:not([type=button], [type=submit], [type=reset], \
       [type=image], [type=file], [type=range], [type=color], [type=hidden])')) \
       throw new Error('that element is a <' + el.localName + '>, not a text field'); \
     el.focus();";

/// For `locate`, after `FOCUS`: select the contents so typing replaces them.
const SELECT_ALL: &str = "if (el.select) el.select(); else document.execCommand('selectAll');";

/// Script fragment: the text of `el`, or what is typed in it if it is a field.
const TEXT_OF: &str = "__kb.text(el)";

/// Whether a page title is a bot check's rather than the site's.
fn is_bot_check(title: &str) -> bool {
    let title = title.to_lowercase();
    [
        "just a moment",
        "client challenge",
        "attention required",
        "captcha",
        "verify you are human",
        "are you a robot",
        "security check",
    ]
    .iter()
    .any(|sign| title.contains(sign))
}

/// Whether a request is the browser's own look for a site icon, which a
/// page under development rarely has and nobody asked for.
fn is_favicon(url: &str) -> bool {
    url.split(['?', '#']).next().is_some_and(|path| path.ends_with("/favicon.ico"))
}

impl Devtools {
    fn park(&mut self, id: u64, pending: Pending) {
        self.pending.insert(id, (pending, Instant::now() + REQUEST_TIMEOUT));
    }

    /// Run one of our own scripts, whose result comes back by value.
    fn evaluate(&mut self, chrome: &mut Chrome, expression: &str, pending: Pending) {
        let id = chrome.request(
            "Runtime.evaluate",
            json!({"expression": expression, "returnByValue": true, "userGesture": true, "awaitPromise": true}),
        );
        self.park(id, pending);
    }

    /// Explain which rules set `property` on an element, from Chrome's
    /// matched styles: the winner first, then those it beat.
    fn cascade(&self, matched: &Value, property: &str) -> String {
        // (selector, value, important, where), in cascade order: later wins.
        let mut found: Vec<(String, String, bool, String)> = Vec::new();
        let declarations = |style: &Value| -> Vec<(String, bool)> {
            let mut out: Vec<(String, bool)> = Vec::new();
            // Chrome lists a declaration both as written and as parsed: keep one of each.
            for p in style["cssProperties"].as_array().into_iter().flatten() {
                if p["name"] != property || p["disabled"] == true || p["parsedOk"] == false {
                    continue;
                }
                let value = p["value"].as_str().unwrap_or("").trim_end_matches("!important").trim().to_owned();
                let declaration = (value, p["important"] == true);
                if !out.contains(&declaration) {
                    out.push(declaration);
                }
            }
            out
        };
        let place = |rule: &Value| -> String {
            if rule["origin"] == "user-agent" {
                return "browser default".to_owned();
            }
            let line = rule["style"]["range"]["startLine"].as_u64();
            match rule["styleSheetId"].as_str().and_then(|id| self.sheets.get(id)) {
                Some((url, start, true)) => {
                    let page = source(url, None);
                    let page = if page.is_empty() { String::new() } else { format!(" of {page}") };
                    format!("a <style>{page}, line {}", line.map_or(1, |l| l + start + 1))
                }
                Some((url, start, false)) => source(url, line.map(|l| l + start)),
                None => "a stylesheet".to_owned(),
            }
        };
        let selector_of = |entry: &Value| -> String {
            let rule = &entry["rule"];
            let all = rule["selectorList"]["selectors"].as_array().cloned().unwrap_or_default();
            let hits: Vec<String> = entry["matchingSelectors"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|i| all.get(i.as_u64()? as usize)?["text"].as_str().map(str::to_owned))
                .collect();
            if hits.is_empty() {
                rule["selectorList"]["text"].as_str().unwrap_or("?").to_owned()
            } else {
                hits.join(", ")
            }
        };
        for entry in matched["matchedCSSRules"].as_array().into_iter().flatten() {
            for (value, important) in declarations(&entry["rule"]["style"]) {
                found.push((selector_of(entry), value, important, place(&entry["rule"])));
            }
        }
        for (value, important) in declarations(&matched["inlineStyle"]) {
            found.push(("style=\"…\" on the element".to_owned(), value, important, "inline".to_owned()));
        }
        if found.is_empty() {
            // Nothing on the element: say where it would inherit from, if anywhere.
            for (depth, ancestor) in matched["inherited"].as_array().into_iter().flatten().enumerate() {
                let rules = ancestor["matchedCSSRules"].as_array().cloned().unwrap_or_default();
                if let Some((entry, value)) =
                    rules.iter().rev().find_map(|e| Some((e, declarations(&e["rule"]["style"]).pop()?.0)))
                {
                    return format!(
                        "no rule sets it on the element itself; it inherits {value} from {} up \
                         (the ancestor matched by {}, {})",
                        plural(depth + 1, "level"),
                        selector_of(entry),
                        place(&entry["rule"])
                    );
                }
            }
            return "no rule sets it on the element or its ancestors: it is the browser's initial value".to_owned();
        }
        // !important beats normal; otherwise the later declaration wins.
        let winner = found.iter().rposition(|d| d.2).unwrap_or(found.len() - 1);
        let mut lines = Vec::new();
        let line = |d: &(String, String, bool, String)| {
            format!("{} {{ {property}: {}{} }}  {}", d.0, d.1, if d.2 { " !important" } else { "" }, d.3)
        };
        lines.push(format!("wins:  {}", line(&found[winner])));
        for (i, d) in found.iter().enumerate().rev() {
            if i != winner {
                lines.push(format!("beaten: {}", line(d)));
            }
        }
        lines.join("\n")
    }

    fn baseline(&self) -> Baseline {
        Baseline {
            seq: self.next_seq,
            failed: self.requests.iter().filter(|r| r.failed()).count(),
            url: self.url.clone(),
            loads: self.loads,
            loader: self.loader.clone(),
        }
    }

    /// What went wrong on the page since `since`, as lines to add to an
    /// answer; empty if nothing did.
    fn fallout(&self, since: &Baseline) -> String {
        let mut out = String::new();
        let base = |url: &str| url.split('#').next().unwrap_or("").to_owned();
        if self.url != since.url && base(&self.url) == base(&since.url) {
            let fragment = self.url.split_once('#').map_or("", |(_, fragment)| fragment);
            let _ = write!(out, "\nmoved within the page, to #{fragment}");
        } else if self.url != since.url {
            let _ = write!(out, "\nthe page is now {}", self.url);
        }
        let errors: Vec<String> =
            self.console.iter().filter(|m| m.seq >= since.seq && m.level == "error").map(Message::line).collect();
        if !errors.is_empty() {
            let _ = write!(out, "\nthis caused {}:\n{}", plural(errors.len(), "console error"), errors.join("\n"));
        }
        for dialog in self.console.iter().filter(|m| m.seq >= since.seq && m.level == DIALOG) {
            let _ = write!(out, "\na dialog appeared: {}", dialog.text);
        }
        let failed = self.requests.iter().filter(|r| r.failed()).count().saturating_sub(since.failed);
        if failed > 0 {
            let _ = write!(out, "\n{} since; see: kb ctl network --failed", plural(failed, "failed request"));
        }
        out
    }

    /// Capture a region of the page, in CSS pixels from the top of the
    /// document, at the size asked for.
    fn capture(&mut self, chrome: &mut Chrome, region: (f64, f64, f64, f64), size: ShotSize, reply: Reply) {
        let (x, y, width, height) = region;
        let mut factor = size.factor(width, self.dpr);
        // Very tall images are refused or shrunk to illegibility by models: keep within bounds.
        if height * factor > MAX_SHOT_HEIGHT {
            factor = MAX_SHOT_HEIGHT / height;
        }
        // Chrome's clip scale multiplies the device pixel ratio.
        let clip = json!({"x": x, "y": y, "width": width, "height": height, "scale": factor / self.dpr});
        let params = json!({"format": "png", "clip": clip, "captureBeyondViewport": true});
        let id = chrome.request("Page.captureScreenshot", params);
        self.park(id, Pending::Image(reply));
    }

    fn errors(&self) -> usize {
        self.page_console().filter(|m| m.level == "error").count()
    }

    fn page_console(&self) -> impl Iterator<Item = &Message> {
        self.console.iter().filter(|m| m.seq >= self.page_seq && m.level != PAGE)
    }

    /// One line on how the page is doing, with where to look if it isn't.
    fn health(&self) -> String {
        let errors = self.errors();
        let failed = self.requests.iter().filter(|r| r.failed()).count();
        let mut line = format!("{}, {}", plural(errors, "console error"), plural(failed, "failed request"));
        if errors > 0 {
            line.push_str("\nsee: kb ctl console --errors");
        }
        if failed > 0 {
            line.push_str("\nsee: kb ctl network --failed");
        }
        line
    }

    /// Carry out one command. The answer goes to `request.reply`, now or
    /// when Chrome has replied.
    pub fn command(&mut self, request: Request, chrome: &mut Chrome, page: Page) {
        let Request { command, args, reply } = request;
        let arg = |name: &str| args[name].as_str().filter(|value| !value.is_empty());
        let flag = |name: &str| args[name] == true || args[name] == "true";
        let need = |name: &str| arg(name).ok_or(format!("{command} needs {name}"));

        let result: Result<(), String> = (|| {
            match command.as_str() {
                "status" => {
                    let (width, height) = page.viewport;
                    let url = if page.url.is_empty() { "about:blank" } else { page.url };
                    let zoom = match page.zoom {
                        zoom if (zoom - 1.0).abs() < 0.001 => String::new(),
                        zoom => format!(", zoomed to {:.0}%", zoom * 100.0),
                    };
                    let mut rest = format!(
                        "loading: {}\nviewport: {width}x{height} CSS pixels, scale {}{zoom}\n{}",
                        if page.loading { "yes" } else { "no" },
                        (page.scale * 1000.0).round() / 1000.0,
                        self.health(),
                    );
                    if !self.throttle.is_empty() {
                        let _ = write!(rest, "\nnetwork throttled: {}", self.throttle);
                    }
                    if let Ok(pane) = std::env::var("REX_BLOCK") {
                        let _ = write!(rest, "\npane: {pane}");
                    }
                    if width < 600 {
                        rest.push_str(
                            "\nnote: the viewport is narrow, so many sites show their phone layout, with menus \
                             collapsed; kb ctl viewport 1280x800 lays pages out as on a desktop",
                        );
                    }
                    // The title has to be asked for; the rest is already known.
                    let pending = Pending::Status(reply.clone(), format!("url: {url}"), rest);
                    self.evaluate(chrome, "document.title", pending);
                }
                "open" => {
                    let url = ui::normalize_url(need("url")?).ok_or("open needs a URL")?;
                    chrome.cache_for(&url);
                    let id = chrome.request("Page.navigate", json!({"url": url}));
                    self.park(id, Pending::Navigate(reply.clone()));
                }
                "reload" => {
                    chrome.reload();
                    self.await_load(reply.clone(), String::new());
                }
                "back" | "forward" => {
                    let step = if command == "back" { -1 } else { 1 };
                    let id = chrome.request("Page.getNavigationHistory", json!({}));
                    self.park(id, Pending::History(reply.clone(), step));
                }
                "screenshot" => {
                    let size = ShotSize::parse(arg("size"))?;
                    self.dpr = page.scale;
                    match arg("selector") {
                        Some(selector) => {
                            let find = format!("{}({})", script(SHOT_RECT), quote(selector));
                            self.evaluate(chrome, &find, Pending::Rect(reply.clone(), Then::Screenshot(size)))
                        }
                        None => {
                            let id = chrome.request("Page.getLayoutMetrics", json!({}));
                            self.park(id, Pending::PageSize(reply.clone(), size, flag("full")));
                        }
                    }
                }
                "console" => {
                    let mut out = String::new();
                    let (all, errors) = (flag("all"), flag("errors"));
                    for message in self.console.iter().filter(|m| all || m.seq >= self.page_seq) {
                        let wanted = match message.level.as_str() {
                            PAGE => all,
                            "error" => true,
                            _ => !errors,
                        };
                        if wanted {
                            out.push_str(&message.line());
                            out.push('\n');
                        }
                    }
                    if !self.page_console().any(|m| !errors || m.level == "error") {
                        let _ = write!(
                            out,
                            "no console {} since the page loaded",
                            if errors { "errors" } else { "messages" }
                        );
                    }
                    text(&reply, out.trim_end());
                }
                "network" => {
                    let mut out = String::new();
                    let failed = flag("failed");
                    for request in self.requests.iter().filter(|r| !failed || r.failed()) {
                        let _ = writeln!(out, "{}", request.line());
                    }
                    if out.is_empty() {
                        out = format!("no {}requests since the page loaded", if failed { "failed " } else { "" });
                    }
                    text(&reply, out.trim_end());
                }
                "eval" => {
                    // By reference, so that a promise or an element can be
                    // told from a plain object; `finish` fetches the rest.
                    let id = chrome.request(
                        "Runtime.evaluate",
                        json!({"expression": need("expression")?, "userGesture": true, "replMode": true}),
                    );
                    self.park(id, Pending::Eval(reply.clone()));
                }
                "style" if flag("why") => {
                    let property =
                        arg("property").ok_or("style --why needs a property, such as: kb ctl style h1 color --why")?;
                    if !self.css_on {
                        self.css_on = true;
                        chrome.request("DOM.enable", json!({}));
                        chrome.request("CSS.enable", json!({}));
                    }
                    chrome.request("DOM.getDocument", json!({"depth": 0}));
                    let selector = need("selector")?;
                    let id = chrome.request(
                        "Runtime.evaluate",
                        json!({"expression": format!("{LIB}\n__kb.get({})", quote(selector))}),
                    );
                    self.park(id, Pending::StyleOf(reply.clone(), selector.to_owned(), property.to_owned()));
                }
                "style" => {
                    let names = match arg("property") {
                        Some(property) => format!("[{}]", quote(property)),
                        None => COMMON_STYLES.to_owned(),
                    };
                    let script = format!(
                        "{LIB}\n(() => {{ const el = __kb.get({sel}); \
                         const style = getComputedStyle(el); \
                         return {names}.map(name => name + ': ' + (style.getPropertyValue(name) || '(not set)')).join('\\n'); }})()",
                        sel = quote(need("selector")?),
                    );
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "text" | "html" => {
                    let value = if command == "text" {
                        "((__kb.visible(el) ? '' : '(hidden: this element is not shown on the page) ') + __kb.text(el))"
                    } else {
                        "el.outerHTML"
                    };
                    let body = match arg("selector") {
                        _ if command == "text" && arg("section").is_some() => format!(
                            "{}({}, {})",
                            names(SECTION),
                            arg("selector").map_or("null".to_owned(), quote),
                            quote(arg("section").unwrap_or(""))
                        ),
                        Some(selector) if flag("all") => format!(
                            "(() => {{ const all = __kb.all({sel}); \
                             if (!all.length) __kb.get({sel}); \
                             return all.map(el => {value}).join('\\n---\\n'); }})()",
                            sel = quote(selector),
                        ),
                        Some(selector) => format!("(() => {{ const el = __kb.get({}); return {value}; }})()", quote(selector)),
                        None if command == "html" => "document.documentElement.outerHTML".to_owned(),
                        // An agent rarely wants the menus and footers: start from the main content.
                        None => "(() => { const el = __kb.main(); const t = __kb.text(el); \
                                 return (el === document.body ? '(the whole page: it marks no <main> or <article>; \
                                 kb ctl outline and kb ctl text --section HEADING read less)' : '(the main content, ' + \
                                 __kb.name(el) + '; for the whole page: kb ctl text body)') + '\\n' + t; })()"
                            .to_owned(),
                    };
                    let script = format!("{LIB}\n{body}");
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "links" => {
                    let limit = arg("limit").and_then(|n| n.parse::<u32>().ok()).unwrap_or(60);
                    let region = arg("selector").map_or("null".to_owned(), quote);
                    let filter = arg("match").map_or("null".to_owned(), quote);
                    let script = format!("{}({region}, {limit}, {filter})", script(LINKS));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "outline" => self.evaluate(chrome, &script(OUTLINE), Pending::Eval(reply.clone())),
                "tables" => {
                    let index = match arg("number") {
                        Some(n) => n
                            .parse::<u32>()
                            .ok()
                            .filter(|n| *n > 0)
                            .ok_or("tables takes a table number, such as 2")?
                            .to_string(),
                        None => "null".to_owned(),
                    };
                    let limit = arg("limit").and_then(|n| n.parse::<u32>().ok()).unwrap_or(200);
                    let script = format!("{}({index}, {limit})", script(TABLES));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "images" => {
                    let region = arg("selector").map_or("null".to_owned(), quote);
                    let script = format!("{}({region}, 20)", script(IMAGES));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "find" => {
                    let limit = arg("limit").and_then(|n| n.parse::<u32>().ok()).unwrap_or(10);
                    let script = format!("{}({}, {limit})", script(FIND), quote(need("text")?));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "box" => {
                    let script = format!("{}({})", script(BOX), quote(need("selector")?));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "scroll" => {
                    let script = format!("{}({})", script(SCROLL), quote(need("to")?));
                    self.evaluate(chrome, &script, Pending::Eval(reply.clone()));
                }
                "select" => {
                    let script =
                        format!("{}({}, {})", script(SELECT), quote(need("selector")?), quote(need("option")?));
                    self.evaluate(chrome, &script, Pending::Acted(reply.clone(), self.baseline()));
                }
                "hover" => {
                    let selector = need("selector")?;
                    let then = Then::Hover(selector.to_owned());
                    self.evaluate(chrome, &locate(selector, &names(CLICKABLE)), Pending::Rect(reply.clone(), then));
                }
                "dialog" => {
                    self.dismiss = match need("answer")? {
                        "accept" => false,
                        "dismiss" => true,
                        other => return Err(format!("dialog takes accept or dismiss, not {other:?}")),
                    };
                    self.prompt_text = arg("text").map(str::to_owned);
                    let answer = if self.dismiss { "dismissed" } else { "accepted" };
                    text(&reply, format!("dialogs will be {answer} from now on"));
                }
                "check" => {
                    // Reported through the same wait as a page load: once quiet.
                    let stopped = (!self.busy).then(Instant::now);
                    let deadline = Instant::now() + LOAD_TIMEOUT;
                    let ignore = arg("ignore").map_or(Vec::new(), |hosts| {
                        hosts.split(',').map(|host| host.trim().to_owned()).filter(|host| !host.is_empty()).collect()
                    });
                    self.loading.push(LoadWait {
                        reply: reply.clone(),
                        said: String::new(),
                        deadline,
                        stopped,
                        check: true,
                        ignore,
                    });
                }
                "storage" => {
                    if flag("clear") {
                        let origin = match self.url.split_once("://") {
                            Some((scheme, rest)) if scheme.starts_with("http") => {
                                format!("{scheme}://{}", rest.split(['/', '?', '#']).next().unwrap_or(""))
                            }
                            _ => return Err("storage --clear works on http and https pages".to_owned()),
                        };
                        let id = chrome.request(
                            "Storage.clearDataForOrigin",
                            json!({
                                "origin": origin,
                                "storageTypes": "cookies,local_storage,session_storage,indexeddb,cache_storage,service_workers",
                            }),
                        );
                        let message =
                            format!("cleared cookies and storage for {origin}; reload to start the page fresh");
                        self.park(id, Pending::Done(reply.clone(), message, self.baseline(), None));
                    } else {
                        self.evaluate(chrome, STORAGE, Pending::Eval(reply.clone()));
                    }
                }
                "listeners" => {
                    let selector = need("selector")?;
                    let id = chrome.request(
                        "Runtime.evaluate",
                        json!({"expression": format!("{LIB}\n__kb.get({})", quote(selector))}),
                    );
                    self.park(id, Pending::ListenersOf(reply.clone(), selector.to_owned()));
                }
                "throttle" => {
                    // (offline, latency ms, download and upload in kilobits a second)
                    let (offline, latency, down, up, name) = match need("speed")? {
                        "off" | "none" => (false, 0.0, -1.0, -1.0, "off"),
                        "slow" | "slow-3g" => (false, 400.0, 400.0, 400.0, "slow: 400 ms round trips, 400 kbit/s"),
                        "fast" | "fast-3g" | "4g" => {
                            (false, 150.0, 1600.0, 750.0, "fast: 150 ms round trips, 1.6 Mbit/s down")
                        }
                        "offline" => (true, 0.0, -1.0, -1.0, "offline"),
                        other => return Err(format!("throttle takes off, slow, fast or offline, not {other:?}")),
                    };
                    let bytes = |kbit: f64| if kbit < 0.0 { -1.0 } else { kbit * 1000.0 / 8.0 };
                    chrome.request(
                        "Network.emulateNetworkConditions",
                        json!({
                            "offline": offline, "latency": latency,
                            "downloadThroughput": bytes(down), "uploadThroughput": bytes(up),
                        }),
                    );
                    self.throttle = if name == "off" { String::new() } else { name.to_owned() };
                    text(
                        &reply,
                        format!("network: {name}{}", if name == "off" { "" } else { "; until kb ctl throttle off" }),
                    );
                }
                "click" => {
                    let selector = need("selector")?;
                    let at = match arg("at") {
                        Some(at) => Some(
                            at.split_once(',')
                                .and_then(|(x, y)| Some((x.trim().parse::<f64>().ok()?, y.trim().parse::<f64>().ok()?)))
                                .ok_or(
                                    "--at takes X,Y: CSS pixels from the element's top left corner, such as 10,20",
                                )?,
                        ),
                        None => None,
                    };
                    let then = Then::Click(selector.to_owned(), if flag("double") { 2 } else { 1 }, at);
                    let prepare = format!("{} {HEAR}", clickable(at));
                    self.evaluate(chrome, &locate(selector, &prepare), Pending::Rect(reply.clone(), then));
                }
                "type" => {
                    let selector = need("selector")?;
                    let then = Then::Type(selector.to_owned(), need("text")?.to_owned());
                    let clear = if flag("clear") { SELECT_ALL } else { "" };
                    let prepare = format!("{} {FOCUS} {clear}", names(CLICKABLE).replace("clicked", "typed into"));
                    self.evaluate(chrome, &locate(selector, &prepare), Pending::Rect(reply.clone(), then));
                }
                "press" => {
                    let name = need("key")?;
                    let key = parse_key(name).ok_or(format!(
                        "don't know the key {name:?}; try Enter, Tab, Escape, ArrowDown, a character, or Ctrl+a"
                    ))?;
                    let since = self.baseline();
                    chrome.key(&key);
                    let message = format!("pressed {name}");
                    self.settling.push(Settling {
                        reply: reply.clone(),
                        message,
                        since,
                        at: Instant::now(),
                        verify: None,
                    });
                }
                "wait" => {
                    let timeout = match arg("timeout") {
                        Some(seconds) => seconds
                            .parse::<f64>()
                            .ok()
                            .filter(|s| *s >= 0.0)
                            .ok_or("--timeout needs a number of seconds")?
                            .min(MAX_WAIT),
                        None => DEFAULT_WAIT,
                    };
                    let selector = need("selector")?;
                    let (what, condition) = match arg("text") {
                        _ if flag("gone") => (format!("{selector} to be gone"), "all.length === 0".to_owned()),
                        _ if arg("count").is_some() => {
                            let count =
                                arg("count").and_then(|n| n.parse::<u32>().ok()).ok_or("--count needs a number")?;
                            (format!("{count} or more of {selector}"), format!("all.length >= {count}"))
                        }
                        Some(wanted) => (
                            format!("{selector} containing {wanted:?}"),
                            format!("all.some(el => __kb.visible(el) && {TEXT_OF}.includes({}))", quote(wanted)),
                        ),
                        None if flag("visible") => {
                            (format!("{selector} to be shown"), "all.some(el => __kb.visible(el))".to_owned())
                        }
                        None => (selector.to_owned(), "all.length > 0".to_owned()),
                    };
                    self.next_wait += 1;
                    self.waits.push(Wait {
                        id: self.next_wait,
                        check: format!(
                            "{LIB}\n(() => {{ const all = __kb.all({}); return {condition}; }})()",
                            quote(selector)
                        ),
                        what,
                        deadline: Instant::now() + Duration::from_secs_f64(timeout),
                        timeout,
                        checking: false,
                        reply: reply.clone(),
                    });
                    self.tick(chrome);
                }
                other => return Err(format!("unknown command {other:?}; see kb ctl --help")),
            }
            Ok(())
        })();
        if let Err(error) = result {
            fail(&reply, error);
        }
    }

    /// Look at a message from Chrome before the app handles it: finish the
    /// command it answers, or log the event it reports.
    pub fn observe(&mut self, message: &Value, chrome: &mut Chrome) {
        if let Some(id) = message["id"].as_u64() {
            if let Some((pending, _)) = self.pending.remove(&id) {
                let result = match message.get("error") {
                    Some(error) => Err(error["message"].as_str().unwrap_or("Chrome refused").to_owned()),
                    None => Ok(&message["result"]),
                };
                self.finish(pending, result, chrome);
            }
            return;
        }
        let params = &message["params"];
        let main_frame = params["frameId"] == chrome.main_frame();
        let method = message["method"].as_str().unwrap_or("");
        if ["Network.", "Runtime.", "Log."].iter().any(|domain| method.starts_with(domain)) {
            self.active = Some(Instant::now());
        }
        match method {
            "Runtime.consoleAPICalled" => {
                let parts: Vec<String> = params["args"].as_array().into_iter().flatten().map(show).collect();
                let frame = &params["stackTrace"]["callFrames"][0];
                let level = match params["type"].as_str().unwrap_or("log") {
                    "warning" => "warning",
                    "error" | "assert" => "error",
                    "info" => "info",
                    "debug" => "debug",
                    _ => "log",
                };
                self.log_at(level, parts.join(" "), frame["url"].as_str().unwrap_or(""), frame["lineNumber"].as_u64());
            }
            "Runtime.exceptionThrown" => {
                let details = &params["exceptionDetails"];
                let thrown = details["exception"]["description"].as_str().or(details["text"].as_str());
                let message = thrown.unwrap_or("uncaught exception").lines().next().unwrap_or("").to_owned();
                let message = message.strip_prefix("Uncaught ").unwrap_or(&message).to_owned();
                let url = details["url"].as_str().unwrap_or("");
                self.log_at("error", format!("Uncaught {message}"), url, details["lineNumber"].as_u64());
            }
            "Log.entryAdded" => {
                let entry = &params["entry"];
                let level = match entry["level"].as_str().unwrap_or("info") {
                    "verbose" => "debug",
                    "warning" => "warning",
                    "error" => "error",
                    _ => "info",
                };
                let url = entry["url"].as_str().unwrap_or("");
                if !is_favicon(url) {
                    self.log_at(
                        level,
                        entry["text"].as_str().unwrap_or("").to_owned(),
                        url,
                        entry["lineNumber"].as_u64(),
                    );
                }
            }
            "CSS.styleSheetAdded" => {
                let header = &params["header"];
                if let Some(id) = header["styleSheetId"].as_str() {
                    let url = header["sourceURL"].as_str().unwrap_or("").to_owned();
                    let start = header["startLine"].as_f64().unwrap_or(0.0) as u64;
                    self.sheets.insert(id.to_owned(), (url, start, header["isInline"] == true));
                }
            }
            "Page.javascriptDialogOpening" => {
                // Answered at once: an open dialog freezes the page, and with
                // it every command.
                let kind = params["type"].as_str().unwrap_or("dialog");
                // Refusing a "leave this page?" would trap the browser on it.
                let accept = !self.dismiss || kind == "beforeunload";
                let typed = self.prompt_text.as_deref().or(params["defaultPrompt"].as_str()).unwrap_or("");
                chrome.request("Page.handleJavaScriptDialog", json!({"accept": accept, "promptText": typed}));
                let mut line = format!(
                    "{kind} {:?} was {}",
                    params["message"].as_str().unwrap_or(""),
                    if accept { "accepted" } else { "dismissed" }
                );
                if kind == "prompt" && accept {
                    let _ = write!(line, " with {typed:?}");
                }
                self.log(DIALOG, line, String::new());
            }
            "Network.requestWillBeSent" => {
                let url = params["request"]["url"].as_str().unwrap_or("");
                let id = params["requestId"].as_str().unwrap_or("");
                let kind = params["type"].as_str().unwrap_or("Other");
                let page = kind == "Document" && main_frame;
                if page {
                    self.document_error = None;
                }
                // A redirect reuses the id: keep one line, for where it ended up.
                self.requests.retain(|request| request.id != id);
                if !url.starts_with("data:") && self.requests.len() < MAX_REQUESTS {
                    self.requests.push(NetRequest {
                        id: id.to_owned(),
                        method: params["request"]["method"].as_str().unwrap_or("GET").to_owned(),
                        url: url.to_owned(),
                        kind: kind.to_ascii_lowercase(),
                        status: None,
                        error: None,
                        finished: false,
                        cached: false,
                        loader: params["loaderId"].as_str().unwrap_or("").to_owned(),
                        page,
                    });
                }
            }
            "Network.responseReceived" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.status = params["response"]["status"].as_u64();
                    request.cached |= params["response"]["fromDiskCache"] == true;
                }
            }
            "Network.requestServedFromCache" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.cached = true;
                }
            }
            "Network.loadingFinished" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.finished = true;
                }
            }
            "Network.loadingFailed" => {
                if let Some(request) = self.request(&params["requestId"]) {
                    request.finished = true;
                    // Requests dropped because the page moved on aren't failures.
                    if params["canceled"] != true {
                        let error = params["errorText"].as_str().unwrap_or("failed").to_owned();
                        request.error = Some(error.clone());
                        if request.page {
                            self.document_error = Some(error);
                        }
                    }
                }
            }
            "Page.frameNavigated" if params["frame"].get("parentId").is_none() => {
                // The new page starts here, when it replaces the old one, and
                // not at its request: a load that fails may replace nothing.
                let loader = params["frame"]["loaderId"].as_str().unwrap_or("");
                if loader != self.loader {
                    self.new_page(loader, params["frame"]["url"].as_str().unwrap_or(""));
                }
                let frame = &params["frame"];
                self.url =
                    format!("{}{}", frame["url"].as_str().unwrap_or(""), frame["urlFragment"].as_str().unwrap_or(""));
                self.moved();
            }
            "Page.navigatedWithinDocument" if main_frame => {
                self.url = params["url"].as_str().unwrap_or("").to_owned();
                self.moved();
            }
            "Page.frameStartedLoading" if main_frame => {
                self.loads += 1;
                self.busy = true;
            }
            "Page.frameStoppedLoading" if main_frame => {
                self.busy = false;
                let now = Instant::now();
                for wait in &mut self.loading {
                    wait.stopped = Some(now);
                }
            }
            _ => {}
        }
    }

    fn log(&mut self, level: &str, text: String, source: String) {
        if self.console.len() == MAX_CONSOLE {
            self.console.pop_front();
        }
        self.console.push_back(Message {
            seq: self.next_seq,
            level: level.to_owned(),
            text,
            source,
            url: String::new(),
        });
        self.next_seq += 1;
    }

    /// Log a message that came from `url`, at a zero-based `line`.
    fn log_at(&mut self, level: &str, text: String, url: &str, line: Option<u64>) {
        self.log(level, text, source(url, line));
        if let Some(message) = self.console.back_mut() {
            message.url = url.to_owned();
        }
    }

    fn request(&mut self, id: &Value) -> Option<&mut NetRequest> {
        let id = id.as_str()?;
        self.requests.iter_mut().rev().find(|request| request.id == id)
    }

    /// The page changed: answer the `back` and `forward` waiting on that.
    fn moved(&mut self) {
        for (reply, _) in std::mem::take(&mut self.moving) {
            text(&reply, format!("now at {}", self.url));
        }
    }

    fn new_page(&mut self, loader: &str, url: &str) {
        self.loader = loader.to_owned();
        self.log(PAGE, url.to_owned(), String::new());
        self.page_seq = self.next_seq;
        // Its own requests began before it took over; the rest were the old page's.
        self.requests.retain(|request| request.loader == loader);
    }

    fn await_load(&mut self, reply: Reply, said: String) {
        let deadline = Instant::now() + LOAD_TIMEOUT;
        self.loading.push(LoadWait { reply, said, deadline, stopped: None, check: false, ignore: Vec::new() });
    }

    /// Tell a command that waited for the page how the load went.
    fn report_load(&mut self, wait: LoadWait, chrome: &mut Chrome) {
        let document = self.requests.iter().find(|request| request.page);
        if self.url.starts_with("chrome-error://") {
            let why = self.document_error.as_deref().unwrap_or("the browser shows an error page");
            return fail(&wait.reply, format!("{}couldn't load the page: {why}", wait.said));
        }
        let refused = match document.and_then(|document| document.status) {
            Some(status) if status >= 400 => format!(", but the server answered {status} for it"),
            _ => String::new(),
        };
        if !wait.check {
            let before = format!("{}loaded {}{refused}", wait.said, self.url);
            let pending = Pending::Status(wait.reply, before, self.health());
            return self.evaluate(chrome, "document.title", pending);
        }
        let ignored = |url: &str| wait.ignore.iter().any(|host| url.contains(host.as_str()));
        let mut problems: Vec<String> = self
            .page_console()
            .filter(|m| {
                m.level == "error" && !ignored(&m.url) && !wait.ignore.iter().any(|host| m.text.contains(host.as_str()))
            })
            .filter(|m| {
                // "Failed to load resource" names no host; it goes with an ignored request.
                !(m.text.starts_with("Failed to load resource")
                    && self.requests.iter().any(|r| {
                        r.failed()
                            && ignored(&r.url)
                            && !m.source.is_empty()
                            && r.url.contains(m.source.split(':').next().unwrap_or("\u{0}"))
                    }))
            })
            .map(Message::line)
            .collect();
        problems.extend(self.requests.iter().filter(|r| r.failed() && !ignored(&r.url)).map(NetRequest::line));
        if problems.is_empty() && refused.is_empty() {
            let except =
                if wait.ignore.is_empty() { String::new() } else { format!(" (ignoring {})", wait.ignore.join(", ")) };
            text(&wait.reply, format!("ok: {}, no console errors, no failed requests{except}", self.url));
        } else {
            if let Some(status) = document.and_then(|document| document.status).filter(|status| *status >= 400) {
                problems.insert(0, format!("the server answered {status} for the page itself"));
            }
            fail(&wait.reply, format!("{} is not healthy:\n{}", self.url, problems.join("\n")));
        }
    }

    fn finish(&mut self, pending: Pending, result: Result<&Value, String>, chrome: &mut Chrome) {
        match pending {
            Pending::Eval(reply) => {
                let object = match evaluated(result) {
                    Ok(object) => object,
                    Err(error) => return fail(&reply, error),
                };
                let description = object["description"].as_str().unwrap_or("object");
                match (object["subtype"].as_str(), &object["objectId"]) {
                    // Wait for it, then look at what it resolved to the same way.
                    (Some("promise"), id @ Value::String(_)) => {
                        let id = chrome.request("Runtime.awaitPromise", json!({"promiseObjectId": id}));
                        self.park(id, Pending::Eval(reply));
                    }
                    (Some("node"), _) => text(&reply, format!("<{description}>")),
                    (None | Some("array"), id @ Value::String(_)) if object["type"] == "object" => {
                        let id = chrome.request(
                            "Runtime.callFunctionOn",
                            json!({
                                "objectId": id, "returnByValue": true,
                                "functionDeclaration": names(CONTENTS),
                            }),
                        );
                        self.park(id, Pending::Contents(reply, description.to_owned()));
                    }
                    _ => text(&reply, show(object)),
                }
            }
            Pending::Contents(reply, description) => match evaluated(result) {
                Ok(object) => text(&reply, show(object)),
                // Cyclic, or otherwise not expressible as JSON.
                Err(_) => text(&reply, description),
            },
            Pending::Navigate(reply) => match result {
                Err(error) => fail(&reply, error),
                Ok(result) => match result["errorText"].as_str() {
                    Some(error) => fail(&reply, format!("couldn't load the page: {error}")),
                    // No new document: only the fragment changed.
                    None if result.get("loaderId").is_none() => text(&reply, "moved within the page"),
                    None => self.await_load(reply, String::new()),
                },
            },
            Pending::Rect(reply, then) => {
                let rect = match evaluated(result) {
                    Ok(object) => &object["value"],
                    Err(error) => return fail(&reply, error),
                };
                let number = |name: &str| rect[name].as_f64().unwrap_or(0.0);
                let (x, y, width, height) = (number("x"), number("y"), number("width"), number("height"));
                if width <= 0.0 || height <= 0.0 {
                    return fail(
                        &reply,
                        "the element has no size, so it isn't visible; it may be in a collapsed menu, or shown \
                         only at other widths (see: kb ctl box, kb ctl viewport)",
                    );
                }
                match then {
                    Then::Hover(selector) => {
                        let since = self.baseline();
                        let id = chrome.request(
                            "Input.dispatchMouseEvent",
                            json!({"type": "mouseMoved", "x": x + width / 2.0, "y": y + height / 2.0}),
                        );
                        self.park(id, Pending::Done(reply, format!("the mouse is over {selector}"), since, None));
                    }
                    Then::Click(selector, count, at) => {
                        let since = self.baseline();
                        let (x, y) = match at {
                            Some((dx, dy)) => (x + dx, y + dy),
                            None => (x + width / 2.0, y + height / 2.0),
                        };
                        // Clicking a container hits whatever is in its middle.
                        let selector = match rect["landed"].as_str() {
                            Some(inner) if !inner.is_empty() => {
                                format!("{selector}; the click landed on {inner} inside it")
                            }
                            _ => selector,
                        };
                        chrome.mouse("mouseMoved", x, y, "none", 0, 0, 0);
                        // A double click is two clicks, the second counted as such.
                        for click in 1..count {
                            chrome.mouse("mousePressed", x, y, "left", 1, click, 0);
                            chrome.mouse("mouseReleased", x, y, "left", 0, click, 0);
                        }
                        chrome.mouse("mousePressed", x, y, "left", 1, count, 0);
                        let id = chrome.request(
                            "Input.dispatchMouseEvent",
                            json!({
                                "type": "mouseReleased", "x": x, "y": y, "button": "left",
                                "buttons": 0, "clickCount": count,
                            }),
                        );
                        let verb = if count == 2 { "double-clicked" } else { "clicked" };
                        let target = rect["target"].as_str().map(str::to_owned);
                        self.park(id, Pending::Done(reply, format!("{verb} {selector}"), since, target));
                    }
                    Then::Type(selector, typed) => {
                        let since = self.baseline();
                        let id = chrome.request("Input.insertText", json!({"text": typed}));
                        self.park(id, Pending::Done(reply, format!("typed into {selector}"), since, None));
                    }
                    Then::Screenshot(size) => self.capture(chrome, (x, y, width, height), size, reply),
                }
            }
            Pending::Heard(action) => match evaluated(result).map(|object| object["value"] == false) {
                Ok(true) => {
                    // Chrome sometimes stops passing mouse input to a page after a form
                    // submission. Click it the way a script would, and say so.
                    let target = action.verify.clone().unwrap_or_default();
                    let script = format!(
                        "{LIB}\n(() => {{ const el = __kb.get({}); \
                         for (const type of ['pointerdown', 'mousedown', 'pointerup', 'mouseup']) \
                           el.dispatchEvent(new (type.startsWith('pointer') ? PointerEvent : MouseEvent)(type, \
                             {{bubbles: true, cancelable: true, composed: true, view: window, button: 0}})); \
                         el.click(); return 'done'; }})()",
                        quote(&target)
                    );
                    let message = format!(
                        "{} (the mouse click didn't reach the page, which Chrome sometimes does after a form \
                         submission, so kb clicked it with JavaScript instead)",
                        action.message
                    );
                    let since = action.since.clone();
                    let id = chrome.request(
                        "Runtime.evaluate",
                        json!({"expression": script, "returnByValue": true, "userGesture": true}),
                    );
                    self.park(id, Pending::Clicked(action.reply, message, since));
                }
                _ => text(&action.reply, format!("{}{}", action.message, self.fallout(&action.since))),
            },
            Pending::StyleOf(reply, selector, property) => match evaluated(result) {
                Ok(object) => {
                    let id = chrome.request("DOM.requestNode", json!({"objectId": object["objectId"]}));
                    self.park(id, Pending::StyleNode(reply, selector, property));
                }
                Err(error) => fail(&reply, error),
            },
            Pending::StyleNode(reply, selector, property) => match result.map(|r| r["nodeId"].as_u64()) {
                Ok(Some(node)) => {
                    let id = chrome.request("CSS.getMatchedStylesForNode", json!({"nodeId": node}));
                    self.park(id, Pending::StyleRules(reply, selector, property, node));
                }
                Ok(None) => fail(&reply, "Chrome couldn't find that element's node"),
                Err(error) => fail(&reply, error),
            },
            Pending::StyleRules(reply, selector, property, node) => match result {
                Ok(matched) => {
                    let explained = self.cascade(matched, &property);
                    let id = chrome.request("CSS.getComputedStyleForNode", json!({"nodeId": node}));
                    self.park(id, Pending::StyleValue(reply, selector, property, explained));
                }
                Err(error) => fail(&reply, error),
            },
            Pending::StyleValue(reply, selector, property, explained) => {
                let value = result
                    .ok()
                    .and_then(|r| r["computedStyle"].as_array().cloned())
                    .and_then(|all| all.into_iter().find(|p| p["name"] == property.as_str()))
                    .and_then(|p| p["value"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "(not a property Chrome knows)".to_owned());
                text(&reply, format!("{property} on {selector}: {value}\n{explained}"));
            }
            Pending::Clicked(reply, message, since) => match evaluated(result) {
                Ok(_) => self.settling.push(Settling { reply, message, since, at: Instant::now(), verify: None }),
                Err(error) => fail(&reply, error),
            },
            Pending::ListenersOf(reply, selector) => match evaluated(result) {
                Ok(object) => {
                    let id = chrome.request("DOMDebugger.getEventListeners", json!({"objectId": object["objectId"]}));
                    self.park(id, Pending::Listeners(reply, selector));
                }
                Err(error) => fail(&reply, error),
            },
            Pending::Listeners(reply, selector) => match result {
                Err(error) => fail(&reply, error),
                Ok(result) => {
                    let listeners = result["listeners"].as_array().cloned().unwrap_or_default();
                    if listeners.is_empty() {
                        return text(
                            &reply,
                            format!(
                                "no event listeners on {selector} itself; one on an ancestor may handle its events \
                                 (try kb ctl listeners body, or a parent)"
                            ),
                        );
                    }
                    let mut out = format!("{} on {selector}:", plural(listeners.len(), "event listener"));
                    for listener in &listeners {
                        let handler = listener["handler"]["description"].as_str().unwrap_or("");
                        let handler: String =
                            handler.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(90).collect();
                        let mut flags = Vec::new();
                        if listener["useCapture"] == true {
                            flags.push("capture");
                        }
                        if listener["once"] == true {
                            flags.push("once");
                        }
                        let flags = if flags.is_empty() { String::new() } else { format!(" ({})", flags.join(", ")) };
                        let _ = write!(out, "\n{}{flags}: {handler}", listener["type"].as_str().unwrap_or("?"));
                    }
                    text(&reply, out);
                }
            },
            Pending::PageSize(reply, size, full) => match result {
                Err(error) => fail(&reply, error),
                Ok(metrics) => {
                    let number = |value: &Value| value.as_f64().unwrap_or(0.0);
                    let region = if full {
                        let page = &metrics["cssContentSize"];
                        (0.0, 0.0, number(&page["width"]), number(&page["height"]))
                    } else {
                        let view = &metrics["cssVisualViewport"];
                        let (x, y) = (number(&view["pageX"]), number(&view["pageY"]));
                        (x, y, number(&view["clientWidth"]), number(&view["clientHeight"]))
                    };
                    self.capture(chrome, region, size, reply);
                }
            },
            Pending::Image(reply) => {
                let _ = reply.send(match result.map(|result| result["data"].as_str()) {
                    Ok(Some(png)) => Ok(Output::Image(png.to_owned())),
                    Ok(None) => Err("Chrome sent no image".to_owned()),
                    Err(error) => Err(error),
                });
            }
            Pending::History(reply, step) => {
                let result = match result {
                    Ok(result) => result,
                    Err(error) => return fail(&reply, error),
                };
                let target = result["currentIndex"].as_i64().unwrap_or(0) + step;
                let entry = usize::try_from(target).ok().and_then(|at| result["entries"].get(at));
                match entry {
                    Some(entry) => {
                        chrome.request("Page.navigateToHistoryEntry", json!({"entryId": entry["id"]}));
                        self.moving.push((reply, Instant::now() + MOVE_TIMEOUT));
                    }
                    None => fail(
                        &reply,
                        format!("there is no {} page to go to", if step < 0 { "earlier" } else { "later" }),
                    ),
                }
            }
            Pending::Done(reply, message, since, verify) => match result {
                Ok(_) => self.settling.push(Settling { reply, message, since, at: Instant::now(), verify }),
                Err(error) => fail(&reply, error),
            },
            Pending::Status(reply, before, after) => {
                let title = evaluated(result).map(show).unwrap_or_default();
                let mut out = format!("{before}\ntitle: {title}\n{after}");
                if is_bot_check(&title) {
                    out.push_str(
                        "\nnote: this looks like a bot check or CAPTCHA, not the page itself; kb can't get past it",
                    );
                }
                text(&reply, out);
            }
            Pending::Acted(reply, since) => match evaluated(result) {
                Ok(object) => self.settling.push(Settling {
                    reply,
                    message: show(object),
                    since,
                    at: Instant::now(),
                    verify: None,
                }),
                Err(error) => fail(&reply, error),
            },
            Pending::WaitCheck(id) => {
                let found = evaluated(result).is_ok_and(|object| object["value"] == true);
                if let Some(at) = self.waits.iter().position(|wait| wait.id == id) {
                    if found {
                        let wait = self.waits.remove(at);
                        text(&wait.reply, format!("waited for {}", wait.what));
                    } else {
                        // Not there yet, or the page was mid-navigation: look again next tick.
                        self.waits[at].checking = false;
                    }
                }
            }
        }
    }

    /// Housekeeping, several times a second: poll waits and give up on
    /// whatever has run out of time.
    pub fn tick(&mut self, chrome: &mut Chrome) {
        let now = Instant::now();
        self.pending.retain(|_, (_, deadline)| *deadline > now);

        for wait in std::mem::take(&mut self.loading) {
            match wait.stopped {
                Some(stopped) => {
                    let quiet_since = self.active.map_or(stopped, |active| active.max(stopped));
                    if now.duration_since(quiet_since) >= QUIET || now.duration_since(stopped) >= QUIET_LIMIT {
                        self.report_load(wait, chrome);
                    } else {
                        self.loading.push(wait);
                    }
                }
                None if wait.deadline <= now => {
                    let seconds = LOAD_TIMEOUT.as_secs();
                    let state = format!("still loading after {seconds}s: {}\n{}", self.url, self.health());
                    text(&wait.reply, format!("{}{state}", wait.said));
                }
                None => self.loading.push(wait),
            }
        }

        self.moving.retain(|(reply, deadline)| {
            if *deadline <= now {
                fail(reply, "the page didn't change");
            }
            *deadline > now
        });

        let (settled, settling): (Vec<_>, Vec<_>) =
            std::mem::take(&mut self.settling).into_iter().partition(|action| {
                let quiet_since = self.active.map_or(action.at, |active| active.max(action.at));
                now.duration_since(quiet_since) >= SETTLE || now.duration_since(action.at) >= SETTLE_LIMIT
            });
        self.settling = settling;
        for action in settled {
            // A new page, not a move within this one (an anchor, a client-side route).
            let new_page = self.loader != action.since.loader || self.busy;
            if self.loads != action.since.loads && new_page {
                // It led to another page: answer when that has loaded and gone quiet.
                let stopped = (!self.busy).then_some(now);
                self.await_load(action.reply, format!("{}\n", action.message));
                if let Some(wait) = self.loading.last_mut() {
                    wait.stopped = stopped;
                }
            } else if action.verify.is_some() && self.url == action.since.url {
                // The same page: make sure the click reached it.
                self.evaluate(chrome, "globalThis.__kbHeard", Pending::Heard(action));
            } else {
                text(&action.reply, format!("{}{}", action.message, self.fallout(&action.since)));
            }
        }

        let mut checks = Vec::new();
        self.waits.retain_mut(|wait| {
            if wait.deadline <= now {
                fail(&wait.reply, format!("gave up after {}s waiting for {}", wait.timeout, wait.what));
                return false;
            }
            if !wait.checking {
                wait.checking = true;
                checks.push((wait.id, wait.check.clone()));
            }
            true
        });
        for (id, script) in checks {
            self.evaluate(chrome, &script, Pending::WaitCheck(id));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shot_sizes() {
        assert_eq!(ShotSize::parse(None).unwrap().factor(1280.0, 2.0), 1.0);
        assert_eq!(ShotSize::parse(Some("small")).unwrap().factor(1280.0, 2.0), 0.5);
        assert_eq!(ShotSize::parse(Some("sharp")).unwrap().factor(1280.0, 2.0), 2.0);
        assert_eq!(ShotSize::parse(Some("640")).unwrap().factor(1280.0, 2.0), 0.5);
        // Asking for more pixels than the screen has gives the screen's.
        assert_eq!(ShotSize::parse(Some("5000px")).unwrap().factor(1280.0, 2.0), 2.0);
        assert!(ShotSize::parse(Some("huge")).is_err());
    }

    #[test]
    fn bot_checks() {
        assert!(is_bot_check("Just a moment..."));
        assert!(is_bot_check("Client Challenge"));
        assert!(!is_bot_check("gap CSS property - CSS | MDN"));
    }

    #[test]
    fn keys() {
        let key = |name| parse_key(name).unwrap();
        assert_eq!(key("Enter").code, KeyCode::Enter);
        assert_eq!(key("ArrowDown").code, KeyCode::Down);
        assert_eq!(key("x").code, KeyCode::Char('x'));
        let select_all = key("Ctrl+a");
        assert!(select_all.ctrl && select_all.code == KeyCode::Char('a'));
        let back_tab = key("Shift+Tab");
        assert!(back_tab.shift && back_tab.code == KeyCode::Tab);
        assert!(parse_key("Hyper+a").is_none());
        assert!(parse_key("Entre").is_none());
    }

    #[test]
    fn sources() {
        assert_eq!(source("https://site.test/js/app.js?v=2", Some(11)), "app.js:12");
        assert_eq!(source("https://site.test/", Some(0)), "site.test:1");
        assert_eq!(source("", Some(3)), "");
    }

    #[test]
    fn shows_values() {
        assert_eq!(show(&json!({"type": "string", "value": "hi"})), "hi");
        assert_eq!(show(&json!({"type": "number", "value": 3})), "3");
        assert_eq!(show(&json!({"type": "object", "value": {"a": [1, 2]}})), r#"{"a":[1,2]}"#);
        assert_eq!(show(&json!({"type": "undefined"})), "undefined");
        let object = json!({"type": "object", "description": "Object", "preview": {"overflow": true, "properties": [
            {"name": "a", "type": "number", "value": "1"}, {"name": "b", "type": "string", "value": "x"},
        ]}});
        assert_eq!(show(&object), r#"{a: 1, b: "x", …}"#);
        let array = json!({"type": "object", "subtype": "array", "preview": {"subtype": "array", "properties": [
            {"name": "0", "type": "number", "value": "1"}, {"name": "1", "type": "number", "value": "2"},
        ]}});
        assert_eq!(show(&array), "[1, 2]");
        assert_eq!(show(&json!({"type": "function", "description": "function f() {}"})), "function f() {}");
    }

    #[test]
    fn thrown_errors_become_one_line() {
        let result = json!({"exceptionDetails": {"exception": {"description": "Error: no element matches #x\n    at <anonymous>:1:1"}}});
        assert_eq!(evaluated(Ok(&result)).unwrap_err(), "no element matches #x");
    }

    #[test]
    fn long_answers_are_cut() {
        let (reply, answer) = std::sync::mpsc::channel();
        text(&reply, "é".repeat(MAX_OUTPUT));
        let Ok(Ok(Output::Text(out))) = answer.recv() else { panic!() };
        assert!(out.len() < MAX_OUTPUT + 100);
        assert!(out.ends_with(&format!("({} more characters not shown)", MAX_OUTPUT / 2)));
    }

    #[test]
    fn console_and_network_follow_the_page() {
        let mut tools = Devtools::default();
        tools.log("error", "old".into(), String::new());
        tools.new_page("loader-1", "https://site.test/");
        tools.log("error", "new".into(), "app.js:3".into());
        tools.log("log", "fine".into(), String::new());
        assert_eq!(tools.errors(), 1);
        assert_eq!(tools.page_console().count(), 2);
        assert_eq!(tools.health(), "1 console error, no failed requests\nsee: kb ctl console --errors");

        // An action's answer carries the errors that followed it, and only those.
        let since = tools.baseline();
        assert_eq!(tools.fallout(&since), "");
        tools.log("error", "Uncaught TypeError: x is null".into(), "app.js:9".into());
        tools.log("log", "noise".into(), String::new());
        assert_eq!(
            tools.fallout(&since),
            "\nthis caused 1 console error:\n[error] Uncaught TypeError: x is null (app.js:9)"
        );
        assert_eq!(tools.console[1].line(), "--- page: https://site.test/ ---");
    }
}
