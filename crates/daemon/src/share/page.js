// Taisce share link: the comment panel. Plain DOM, no dependencies; every
// piece of text goes in through textContent, never innerHTML. The page is
// sandboxed into an opaque origin (CSP `sandbox`), so storage may throw and
// every fetch is a credential-less CORS request with Origin: null.
(function () {
  "use strict";
  if (document.body.getAttribute("data-comments") !== "on") return;
  var api = location.pathname.replace(/\/+$/, "") + "/comments";
  var threadsEl = document.getElementById("threads");
  var doc = document.getElementById("doc");
  var NAME_KEY = "taisce.share.name";
  var comments = [];

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text != null) e.textContent = text;
    return e;
  }
  function getName() { try { return localStorage.getItem(NAME_KEY) || ""; } catch (e) { return ""; } }
  function setName(n) { try { localStorage.setItem(NAME_KEY, n); } catch (e) { /* private mode */ } }
  function when(iso) {
    var d = new Date(iso);
    return isNaN(d) ? "" : d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
  }
  function block(i) { return doc.querySelector('[data-b="' + Number(i) + '"]'); }

  // a form: name (remembered), body, a honeypot no person fills in
  function form(opts, done) {
    var f = el("form", "cf");
    var name = el("input");
    name.placeholder = "Your name"; name.maxLength = 60; name.required = true; name.value = getName();
    name.autocomplete = "name";
    var body = el("textarea");
    body.placeholder = opts.reply ? "Reply" : "Comment"; body.maxLength = 4000; body.required = true;
    var hp = el("div", "hp");
    var site = el("input"); site.name = "website"; site.tabIndex = -1; site.autocomplete = "off";
    hp.setAttribute("aria-hidden", "true");
    hp.appendChild(site);
    var row = el("div", "row");
    var err = el("span", "err");
    var cancel = el("button", null, "Cancel"); cancel.type = "button";
    var send = el("button", "primary", opts.reply ? "Reply" : "Comment"); send.type = "submit";
    row.append(err, cancel, send);
    f.append(name, body, hp, row);
    cancel.addEventListener("click", function () { f.remove(); if (opts.onCancel) opts.onCancel(); });
    f.addEventListener("submit", function (ev) {
      ev.preventDefault();
      var n = name.value.trim(), b = body.value.trim();
      if (!n || !b) { err.textContent = "Name and comment are both needed."; return; }
      send.disabled = true; err.textContent = "";
      setName(n);
      var payload = { name: n, body: b, website: site.value };
      if (opts.anchor) payload.anchor = opts.anchor;
      if (opts.parent) payload.parent_id = opts.parent;
      fetch(api, { method: "POST", credentials: "omit", headers: { "content-type": "application/json" }, body: JSON.stringify(payload) })
        .then(function (r) {
          if (r.status === 201) { f.remove(); done(); return; }
          return r.json().catch(function () { return {}; }).then(function (j) {
            err.textContent = r.status === 429 ? "Too many comments for now; try again later." : (j.error || "Could not send that.");
            send.disabled = false;
          });
        })
        .catch(function () { err.textContent = "Offline? Could not send that."; send.disabled = false; });
    });
    setTimeout(function () { (name.value ? body : name).focus(); }, 0);
    return f;
  }

  function render() {
    threadsEl.textContent = "";
    var marked = doc.querySelectorAll(".has-comments");
    for (var m = 0; m < marked.length; m++) marked[m].classList.remove("has-comments");
    var roots = comments.filter(function (c) { return !c.parent_id; });
    if (!roots.length) threadsEl.appendChild(el("p", "empty-note", "No comments yet. Select text in the document to comment on it."));
    roots.forEach(function (root) {
      var t = el("div", "thread");
      if (root.anchor && root.anchor.quote) {
        var q = el("div", "quote", root.anchor.quote);
        var b = block(root.anchor.block);
        if (b) {
          b.classList.add("has-comments");
          q.addEventListener("click", function () {
            b.scrollIntoView({ behavior: "smooth", block: "center" });
            b.classList.add("flash");
            setTimeout(function () { b.classList.remove("flash"); }, 1200);
          });
        }
        t.appendChild(q);
      }
      [root].concat(comments.filter(function (c) { return c.parent_id === root.id; })).forEach(function (c) {
        var d = el("div", "c");
        d.appendChild(el("span", "who", c.author));
        if (c.is_owner) d.appendChild(el("span", "owner", "author"));
        d.appendChild(el("span", "when", when(c.created_at)));
        d.appendChild(el("div", "text", c.body));
        t.appendChild(d);
      });
      var reply = el("button", "link", "Reply"); reply.type = "button";
      reply.addEventListener("click", function () {
        reply.hidden = true;
        t.appendChild(form({ reply: true, parent: root.id, onCancel: function () { reply.hidden = false; } }, load));
      });
      t.appendChild(reply);
      threadsEl.appendChild(t);
    });
    var general = el("button", "link", "Add a comment"); general.type = "button";
    general.addEventListener("click", function () {
      general.hidden = true;
      threadsEl.appendChild(form({ onCancel: function () { general.hidden = false; } }, load));
    });
    threadsEl.appendChild(general);
  }

  function load() {
    fetch(api, { credentials: "omit", headers: { accept: "application/json" } })
      .then(function (r) { return r.ok ? r.json() : { comments: [], enabled: false }; })
      .then(function (j) {
        if (j.enabled === false) { var p = document.getElementById("comments"); if (p) p.remove(); return; }
        comments = j.comments || [];
        render();
      })
      .catch(function () { /* offline: the doc still reads */ });
  }

  // select text inside one block → a floating "Comment" button
  var btn = el("button", "primary", "Comment"); btn.id = "comment-btn"; btn.type = "button"; btn.hidden = true;
  document.body.appendChild(btn);
  var pending = null;
  function selectionAnchor() {
    var sel = window.getSelection();
    if (!sel || sel.isCollapsed || !sel.rangeCount) return null;
    var r = sel.getRangeAt(0);
    var node = r.commonAncestorContainer;
    if (node.nodeType !== 1) node = node.parentElement;
    var b = node && node.closest ? node.closest("[data-b]") : null;
    if (!b || !doc.contains(b)) return null;
    var quote = sel.toString().replace(/\s+/g, " ").trim().slice(0, 500);
    if (!quote) return null;
    return { anchor: { block: Number(b.getAttribute("data-b")), quote: quote }, rect: r.getBoundingClientRect() };
  }
  function onSelect() {
    var a = selectionAnchor();
    if (!a) { btn.hidden = true; return; }
    pending = a.anchor;
    btn.style.top = (window.scrollY + a.rect.bottom + 8) + "px";
    btn.style.left = Math.max(8, Math.min(window.scrollX + a.rect.left, window.scrollX + document.documentElement.clientWidth - 110)) + "px";
    btn.hidden = false;
  }
  document.addEventListener("selectionchange", function () { setTimeout(onSelect, 0); });
  btn.addEventListener("mousedown", function (e) { e.preventDefault(); });
  btn.addEventListener("click", function () {
    if (!pending) return;
    var anchor = pending;
    btn.hidden = true;
    var t = el("div", "thread");
    t.appendChild(el("div", "quote", anchor.quote));
    t.appendChild(form({ anchor: anchor, onCancel: function () { t.remove(); } }, load));
    threadsEl.insertBefore(t, threadsEl.firstChild);
    t.scrollIntoView({ behavior: "smooth", block: "nearest" });
  });

  load();
})();
