// The web UI's interactivity: a search combobox, an outline dialog built
// from the page's own headings, a sticky breadcrumb, and a small keyboard
// map. Everything here is additive — the search form, result links and
// article links all work with this file entirely absent.
"use strict";

const input = document.getElementById("search-input");
const suggestions = document.getElementById("suggestions");
const breadcrumb = document.getElementById("breadcrumb");
const live = document.getElementById("live");
const outlineDialog = document.getElementById("outline");
const helpDialog = document.getElementById("help");
const article = document.getElementById("article");

// ---------------------------------------------------------------------
// Pure logic: filtering the outline and picking the next/previous link.
// Kept free of the DOM so they're inspectable and reviewable on their own.
// ---------------------------------------------------------------------

function normalizeText(s) {
  return s.normalize("NFKD").replace(/[̀-ͯ]/g, "").toLowerCase();
}

/// Mirrors outline.rs::refilter: every word in the query must appear
/// somewhere in the heading; the first heading with a word that *starts*
/// with the first query word sorts first, ties broken by document order.
function filterOutline(sections, query) {
  const words = normalizeText(query).split(" ").filter(Boolean);
  if (words.length === 0) return sections.map((_, i) => i);
  const hits = [];
  let prefixHit = -1;
  sections.forEach((s, i) => {
    const heading = normalizeText(s.heading);
    if (words.every((w) => heading.includes(w))) {
      if (prefixHit === -1 && heading.split(" ").some((h) => h.startsWith(words[0]))) {
        prefixHit = hits.length;
      }
      hits.push(i);
    }
  });
  if (prefixHit > 0) {
    const [picked] = hits.splice(prefixHit, 1);
    hits.unshift(picked);
  }
  return hits;
}

/// `tops` is each link's distance from the top of the document, in reading
/// order. Mirrors the TUI's select_link: from nothing selected, forward
/// picks the first link at/after `viewportTop`; backward picks the last one
/// still above `viewportTop + viewportHeight`. From a selection still
/// visible, it just steps by one.
function nextLinkIndex(tops, viewportTop, viewportHeight, current, forward) {
  if (tops.length === 0) return -1;
  const visible = (i) => tops[i] >= viewportTop && tops[i] < viewportTop + viewportHeight;
  if (current >= 0 && visible(current)) {
    return forward ? Math.min(current + 1, tops.length - 1) : Math.max(current - 1, 0);
  }
  if (forward) {
    const i = tops.findIndex((t) => t >= viewportTop);
    return i === -1 ? tops.length - 1 : i;
  }
  for (let i = tops.length - 1; i >= 0; i--) {
    if (tops[i] < viewportTop + viewportHeight) return i;
  }
  return 0;
}

// ---------------------------------------------------------------------
// Search combobox
// ---------------------------------------------------------------------

let suggestAbort = null;
let activeIndex = -1;

function announce(message) {
  live.textContent = message;
}

function closeSuggestions() {
  suggestions.hidden = true;
  suggestions.innerHTML = "";
  activeIndex = -1;
  input.setAttribute("aria-expanded", "false");
  input.removeAttribute("aria-activedescendant");
}

function renderSuggestions(items, query) {
  suggestions.innerHTML = "";
  items.forEach((item, i) => {
    const li = document.createElement("li");
    li.id = `suggestion-${i}`;
    li.setAttribute("role", "option");
    li.dataset.href = `/wiki/${encodeURIComponent(item.path).replace(/%2F/g, "/")}${item.fragment ? "#" + encodeURIComponent(item.fragment) : ""}`;
    li.textContent = item.matched ? `${item.matched} → ${item.title}` : item.title;
    suggestions.appendChild(li);
  });
  const searchAll = document.createElement("li");
  searchAll.id = `suggestion-${items.length}`;
  searchAll.setAttribute("role", "option");
  searchAll.dataset.href = `/search?q=${encodeURIComponent(query)}`;
  searchAll.textContent = `Search all text for "${query}"`;
  suggestions.appendChild(searchAll);
  suggestions.hidden = false;
  input.setAttribute("aria-expanded", "true");
  activeIndex = -1;
  announce(`${items.length} suggestion${items.length === 1 ? "" : "s"}`);
}

async function fetchSuggestions(query) {
  if (suggestAbort) suggestAbort.abort();
  if (!query.trim()) {
    closeSuggestions();
    return;
  }
  suggestAbort = new AbortController();
  try {
    const res = await fetch(`/api/suggest?q=${encodeURIComponent(query)}`, { signal: suggestAbort.signal });
    if (!res.ok) throw new Error(String(res.status));
    renderSuggestions(await res.json(), query);
  } catch (err) {
    if (err.name !== "AbortError") announce("couldn't refresh suggestions, showing previous results");
  }
}

input.addEventListener("input", () => fetchSuggestions(input.value));

function moveActive(delta) {
  const options = suggestions.querySelectorAll('[role="option"]');
  if (options.length === 0) return;
  activeIndex = (activeIndex + delta + options.length) % options.length;
  options.forEach((o, i) => o.setAttribute("aria-selected", String(i === activeIndex)));
  input.setAttribute("aria-activedescendant", options[activeIndex].id);
}

function openActive() {
  const options = suggestions.querySelectorAll('[role="option"]');
  if (activeIndex >= 0 && options[activeIndex]) {
    window.location.href = options[activeIndex].dataset.href;
  }
}

suggestions.addEventListener("mousedown", (e) => {
  const option = e.target.closest('[role="option"]');
  if (option) window.location.href = option.dataset.href;
});

input.addEventListener("keydown", (e) => {
  if (e.key === "ArrowDown") {
    e.preventDefault();
    moveActive(1);
  } else if (e.key === "ArrowUp") {
    e.preventDefault();
    moveActive(-1);
  } else if (e.key === "Enter" && activeIndex >= 0) {
    e.preventDefault();
    openActive();
  } else if (e.key === "Escape") {
    closeSuggestions();
  }
});

// ---------------------------------------------------------------------
// Outline dialog: built from this page's own headings, not a server call.
// ---------------------------------------------------------------------

function articleSections() {
  if (!article) return [];
  return Array.from(article.querySelectorAll("h1[id], h2[id], h3[id], h4[id], h5[id], h6[id]")).map((h) => ({
    id: h.id,
    level: Number(h.tagName[1]),
    heading: h.textContent,
    path: h.dataset.path || h.textContent,
  }));
}

let outlineQuery = "";
let currentHeadingId = null;

// Real ZIM anchor ids can contain a `"` (e.g. Definition_of_"racial_discrimination"),
// which survives into `s.id`/`s.path` as-is once the browser decodes the
// server's escaped attribute. Rows are built with createElement/setAttribute
// rather than interpolated into an innerHTML string, so that character can
// never break out of an attribute value.
function renderOutline(sections, matches) {
  const list = document.createElement("ul");
  list.id = "outline-list";
  for (const i of matches) {
    const s = sections[i];
    const a = document.createElement("a");
    a.href = `#${s.id}`;
    a.dataset.closeOutline = "";
    if (s.id === currentHeadingId) a.setAttribute("aria-current", "true");
    const indent = " ".repeat(Math.max(0, s.level - 2));
    a.textContent = `${indent}${s.path}`;
    const li = document.createElement("li");
    li.appendChild(a);
    list.appendChild(li);
  }
  outlineDialog.innerHTML = `<input type="text" id="outline-filter" placeholder="Filter sections" autocomplete="off">`;
  outlineDialog.appendChild(list);
  outlineDialog.querySelector("#outline-filter").value = outlineQuery;
  outlineDialog.querySelector("[aria-current]")?.scrollIntoView({ block: "center" });
}

function openOutline() {
  const sections = articleSections();
  if (sections.length === 0) return;
  outlineQuery = "";
  renderOutline(sections, filterOutline(sections, outlineQuery));
  outlineDialog.showModal();
  outlineDialog.querySelector("#outline-filter").focus();
}

outlineDialog.addEventListener("input", (e) => {
  if (e.target.id !== "outline-filter") return;
  outlineQuery = e.target.value;
  const sections = articleSections();
  renderOutline(sections, filterOutline(sections, outlineQuery));
});

outlineDialog.addEventListener("keydown", (e) => {
  if (e.key === "Enter") {
    const first = outlineDialog.querySelector("#outline-list a");
    if (first) {
      e.preventDefault();
      window.location.hash = first.getAttribute("href");
      outlineDialog.close();
    }
  }
});

// Escape fires `cancel` before the dialog closes: the first Escape clears a
// non-empty filter instead of closing, the second (query now empty) closes
// natively.
outlineDialog.addEventListener("cancel", (e) => {
  if (outlineQuery) {
    e.preventDefault();
    outlineQuery = "";
    const sections = articleSections();
    renderOutline(sections, filterOutline(sections, outlineQuery));
    outlineDialog.querySelector("#outline-filter").focus();
  }
});

outlineDialog.addEventListener("click", (e) => {
  if (e.target.closest("[data-close-outline]")) outlineDialog.close();
});

// ---------------------------------------------------------------------
// Sticky breadcrumb via IntersectionObserver
// ---------------------------------------------------------------------

if (article && "IntersectionObserver" in window) {
  const headings = Array.from(article.querySelectorAll("h1[data-path], h2[data-path], h3[data-path], h4[data-path], h5[data-path], h6[data-path]"));
  const observer = new IntersectionObserver(
    (entries) => {
      const visible = entries.filter((e) => e.isIntersecting).sort((a, b) => a.boundingClientRect.top - b.boundingClientRect.top);
      if (visible.length > 0) {
        breadcrumb.textContent = visible[0].target.dataset.path;
        currentHeadingId = visible[0].target.id;
      }
    },
    { rootMargin: "0px 0px -80% 0px" }
  );
  headings.forEach((h) => observer.observe(h));
}

// ---------------------------------------------------------------------
// n/N: focus the next/previous link at or below the viewport top.
// ---------------------------------------------------------------------

let selectedLinkIndex = -1;

function articleLinks() {
  return article ? Array.from(article.querySelectorAll("a")) : [];
}

// Each `n`/`N` press recomputed the whole link list and its top offset via
// getBoundingClientRect on every one of them — over a thousand forced
// layout reads on the largest article, on every keypress. Cached here,
// invalidated only on resize (a link's on-page position is otherwise
// stable between presses).
let linkCache = null;

function getLinkCache() {
  if (!linkCache) {
    const links = articleLinks();
    const tops = links.map((l) => l.getBoundingClientRect().top + window.scrollY);
    linkCache = { links, tops };
  }
  return linkCache;
}

window.addEventListener("resize", () => {
  linkCache = null;
});

function focusAdjacentLink(forward) {
  const { links, tops } = getLinkCache();
  if (links.length === 0) return;
  const next = nextLinkIndex(tops, window.scrollY, window.innerHeight, selectedLinkIndex, forward);
  selectedLinkIndex = next;
  links[next].focus();
  links[next].scrollIntoView({ block: "nearest" });
}

// ---------------------------------------------------------------------
// The keyboard map. Ignored in text fields, in an open dialog, or with a
// modifier held.
// ---------------------------------------------------------------------

document.addEventListener("keydown", (e) => {
  // The live region's message (a suggestion count, a fetch-failure note) is
  // stale past the keypress that follows it (critique #12).
  live.textContent = "";
  if (e.ctrlKey || e.metaKey || e.altKey) return;
  const inField = document.activeElement && ["INPUT", "TEXTAREA"].includes(document.activeElement.tagName);
  const dialogOpen = document.querySelector("dialog[open]");
  if (dialogOpen) {
    if (e.key === "?" && dialogOpen === helpDialog) helpDialog.close();
    return;
  }
  if (inField) return;
  switch (e.key) {
    case "/":
      e.preventDefault();
      input.focus();
      input.select();
      break;
    case "o":
      e.preventDefault();
      openOutline();
      break;
    case "n":
      e.preventDefault();
      focusAdjacentLink(true);
      break;
    case "N":
      e.preventDefault();
      focusAdjacentLink(false);
      break;
    case "r":
      window.location.href = "/random";
      break;
    case "?":
      e.preventDefault();
      helpDialog.showModal();
      break;
  }
});

// ---------------------------------------------------------------------
// Browser-side timing (critique #18): always recorded as a data attribute,
// only logged when the page was loaded with ?perf.
// ---------------------------------------------------------------------

document.addEventListener("DOMContentLoaded", () => {
  const dclMs = performance.now();
  requestAnimationFrame(() => {
    const rafMs = performance.now();
    document.documentElement.dataset.perfReady = JSON.stringify({ dcl: dclMs, raf: rafMs });
    if (new URLSearchParams(window.location.search).has("perf")) {
      console.log(`DOMContentLoaded ${dclMs.toFixed(1)}ms, first paint frame ${rafMs.toFixed(1)}ms`);
    }
  });
});
