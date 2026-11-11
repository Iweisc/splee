// Served as /api/c/{key}.js. It reads its settings from the same address with /config in place of .js, keeps the conversation
// in the site's localStorage so it follows the visitor across pages, and polls for replies while it's open. Every request is
// a GET or a text/plain POST, so browsers never send a preflight. It draws inside a shadow root, so the site's CSS can't
// reach it. Without a document (node --test) it only hands out the pure functions below.
(function () {
  "use strict";

  /** Whether the widget shows on `path`: every page when `paths` is empty, else a listed page or one under it. */
  function showsOn(paths, path) {
    if (!paths || !paths.length) return true;
    var clean = (path || "/").replace(/\/+$/, "") || "/";
    return paths.some(function (p) {
      return clean === p || (p !== "/" && clean.indexOf(p + "/") === 0);
    });
  }

  /** Text split into plain runs and links, so links can be drawn as anchors without parsing HTML. */
  function parts(text) {
    var out = [];
    var re = /https?:\/\/[^\s<>"]+[^\s<>".,;:!?)]/g;
    var at = 0;
    var m;
    while ((m = re.exec(text))) {
      if (m.index > at) out.push({ text: text.slice(at, m.index) });
      out.push({ link: m[0] });
      at = m.index + m[0].length;
    }
    if (at < text.length) out.push({ text: text.slice(at) });
    return out;
  }

  /** `have` with the messages in `fresh` it doesn't have yet, in order, keeping the last `keep`. */
  function merge(have, fresh, keep) {
    var ids = {};
    have.forEach(function (m) {
      ids[m.id] = 1;
    });
    var all = have.concat(
      fresh.filter(function (m) {
        return !ids[m.id];
      }),
    );
    all.sort(function (a, b) {
      return a.id - b.id;
    });
    return all.slice(-keep);
  }

  if (typeof document === "undefined") {
    globalThis.spleeChat = { showsOn: showsOn, parts: parts, merge: merge };
    return;
  }

  var script = document.currentScript;
  if (!script || window.spleeChatLoaded) return;
  window.spleeChatLoaded = true;
  var base = script.src.replace(/\.js(\?.*)?$/, "");
  var storeKey = "splee_chat:" + base.split("/").pop();
  var state = { m: [] };
  try {
    state = JSON.parse(localStorage.getItem(storeKey) || "") || state;
  } catch (e) {}
  function save() {
    try {
      localStorage.setItem(storeKey, JSON.stringify(state));
    } catch (e) {}
  }
  function post(path, body) {
    return fetch(base + path, { method: "POST", body: JSON.stringify(body) }).then(function (r) {
      return r.json().then(function (j) {
        if (!r.ok) throw new Error(j.error || "Something went wrong. Try again.");
        return j;
      });
    });
  }

  fetch(base + "/config")
    .then(function (r) {
      return r.ok ? r.json() : null;
    })
    .then(function (cfg) {
      if (cfg && cfg.enabled && showsOn(cfg.paths, location.pathname)) start(cfg);
    })
    .catch(function () {});

  function el(tag, cls, text) {
    var n = document.createElement(tag);
    if (cls) n.className = cls;
    if (text) n.textContent = text;
    return n;
  }

  function start(cfg) {
    var side = cfg.position === "left" ? "left" : "right";
    var host = el("div");
    host.style.cssText = "position:fixed;z-index:2147483000;bottom:0;" + side + ":0";
    var root = host.attachShadow({ mode: "open" });
    var css = el("style");
    css.textContent =
      ":host{all:initial}*{box-sizing:border-box;font:14px/1.45 system-ui,-apple-system,'Segoe UI',sans-serif}" +
      ".b{position:fixed;bottom:20px;" + side + ":20px;width:56px;height:56px;border-radius:50%;border:0;cursor:pointer;" +
      "background:" + cfg.accent + ";color:#fff;box-shadow:0 6px 24px rgba(0,0,0,.2);display:grid;place-items:center}" +
      ".b svg{width:26px;height:26px}.dot{position:absolute;top:4px;right:4px;width:12px;height:12px;border-radius:50%;" +
      "background:#e11d48;border:2px solid #fff}" +
      ".p{position:fixed;bottom:88px;" + side + ":20px;width:370px;height:min(560px,calc(100vh - 110px));background:#fff;" +
      "color:#111;border-radius:16px;box-shadow:0 12px 48px rgba(0,0,0,.22);display:flex;flex-direction:column;overflow:hidden}" +
      ".h{background:" + cfg.accent + ";color:#fff;padding:14px 16px;display:flex;align-items:center;gap:8px}" +
      ".h b{flex:1;font-weight:600;font-size:15px}.x{background:none;border:0;color:#fff;cursor:pointer;font-size:22px;line-height:1;padding:0 4px}" +
      ".l{flex:1;overflow-y:auto;padding:14px;display:flex;flex-direction:column;gap:8px;background:#f7f7f8}" +
      ".m{max-width:85%;padding:8px 12px;border-radius:14px;white-space:pre-wrap;overflow-wrap:anywhere}" +
      ".v{align-self:flex-end;background:" + cfg.accent + ";color:#fff;border-bottom-right-radius:4px}" +
      ".a{align-self:flex-start;background:#fff;border:1px solid #e5e5e5;border-bottom-left-radius:4px}" +
      ".a a{color:inherit;text-decoration:underline}.v a{color:#fff}" +
      ".who{display:block;font-size:11px;color:#666;margin-bottom:2px}" +
      ".t{align-self:flex-start;color:#888;padding:4px 2px}.err{align-self:center;color:#b91c1c;font-size:13px;text-align:center}" +
      ".f{display:flex;gap:8px;padding:10px;border-top:1px solid #eee;background:#fff}" +
      ".f textarea{flex:1;resize:none;border:1px solid #ddd;border-radius:10px;padding:8px 10px;height:40px;max-height:120px;outline:none}" +
      ".f textarea:focus,.e input:focus{border-color:" + cfg.accent + "}" +
      ".s{border:0;border-radius:10px;padding:0 14px;background:" + cfg.accent + ";color:#fff;cursor:pointer;font-weight:600}" +
      ".s:disabled{opacity:.5;cursor:default}" +
      ".e{align-self:stretch;background:#fff;border:1px solid #e5e5e5;border-radius:12px;padding:12px;display:flex;flex-direction:column;gap:8px}" +
      ".e p{margin:0}.e input{border:1px solid #ddd;border-radius:8px;padding:8px 10px;outline:none;width:100%}.e .s{height:36px}" +
      ".k{background:none;border:0;padding:6px 0 10px;color:#666;font-size:12px;cursor:pointer;text-decoration:underline;align-self:center}" +
      "@media (max-width:480px){.p{left:0;right:0;bottom:0;top:0;width:auto;height:auto;border-radius:0}.b.o{display:none}}";
    root.appendChild(css);

    var panel = el("div", "p");
    panel.setAttribute("role", "dialog");
    panel.setAttribute("aria-label", cfg.title);
    var head = el("div", "h");
    head.appendChild(el("b", "", cfg.title));
    var close = el("button", "x", "×");
    close.setAttribute("aria-label", "Close chat");
    head.appendChild(close);
    var list = el("div", "l");
    list.setAttribute("aria-live", "polite");
    var talk = el("button", "k", "Talk to a person");
    var form = el("form", "f");
    var input = el("textarea");
    input.placeholder = "Type your question";
    input.maxLength = cfg.max_length;
    input.setAttribute("aria-label", "Your message");
    var send = el("button", "s", "Send");
    form.appendChild(input);
    form.appendChild(send);
    panel.appendChild(head);
    panel.appendChild(list);
    panel.appendChild(form);

    var button = el("button", "b");
    button.setAttribute("aria-label", "Open chat");
    button.innerHTML =
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M21 15a2 2 0 0 1-2 2H7l-4 4V5a2 2 0 0 1 2-2h14a2 2 0 0 1 2 2z"/></svg>';
    var dot = el("span", "dot");
    root.appendChild(panel);
    root.appendChild(button);
    document.body.appendChild(host);

    var busy = false;
    var error = "";
    var timer;

    function bubble(m) {
      var node = el("div", "m " + (m.role === "visitor" ? "v" : "a"));
      if (m.role === "team" && m.who) node.appendChild(el("span", "who", m.who));
      parts(m.body).forEach(function (p) {
        if (p.text) return node.appendChild(document.createTextNode(p.text));
        var a = el("a", "", p.link);
        a.href = p.link;
        a.target = "_blank";
        a.rel = "noopener";
        node.appendChild(a);
      });
      return node;
    }

    function emailForm() {
      var box = el("form", "e");
      box.appendChild(el("p", "", "Leave your email and someone from the team will get back to you."));
      var fields = [
        ["email", "Email", "email"],
        ["name", "Name (optional)", "text"],
        ["company", "Company (optional)", "text"],
      ].map(function (f) {
        var i = el("input");
        i.name = f[0];
        i.placeholder = f[1];
        i.type = f[2];
        i.maxLength = 200;
        i.setAttribute("aria-label", f[1]);
        box.appendChild(i);
        return i;
      });
      fields[0].required = true;
      var go = el("button", "s", "Send");
      box.appendChild(go);
      box.onsubmit = function (e) {
        e.preventDefault();
        go.disabled = true;
        post("/lead", { conversation: state.c, token: state.t, email: fields[0].value, name: fields[1].value, company: fields[2].value })
          .then(function (j) {
            state.ask = false;
            state.lead = true;
            take(j.messages);
          })
          .catch(function (err) {
            error = err.message;
            go.disabled = false;
            draw();
          });
      };
      return box;
    }

    function draw() {
      list.textContent = "";
      list.appendChild(bubble({ id: 0, role: "bot", body: cfg.greeting }));
      state.m.forEach(function (m) {
        list.appendChild(bubble(m));
      });
      if (busy) list.appendChild(el("div", "t", "Typing…"));
      if (state.ask && !state.lead) list.appendChild(emailForm());
      else if (!state.lead && state.c) list.appendChild(talk);
      if (error) list.appendChild(el("div", "err", error));
      list.scrollTop = list.scrollHeight;
      send.disabled = busy;
      panel.style.display = state.open ? "flex" : "none";
      button.setAttribute("aria-expanded", state.open ? "true" : "false");
      button.classList.toggle("o", !!state.open);
      if (state.unread && !state.open) button.appendChild(dot);
      else if (dot.parentNode) dot.remove();
    }

    function take(fresh) {
      var before = state.m.length;
      state.m = merge(state.m, fresh || [], 100);
      if (state.m.length > before && !state.open) state.unread = true;
      save();
      draw();
    }

    function poll() {
      clearTimeout(timer);
      if (state.c && !document.hidden) {
        var last = state.m.length ? state.m[state.m.length - 1].id : 0;
        post("/poll", { conversation: state.c, token: state.t, after: last })
          .then(function (j) {
            take(j.messages);
          })
          .catch(function () {});
      }
      if (state.c) timer = setTimeout(poll, state.open ? 4000 : 30000);
    }

    form.onsubmit = function (e) {
      e.preventDefault();
      var text = input.value.trim();
      if (!text || busy) return;
      busy = true;
      error = "";
      input.value = "";
      var pending = { id: Number.MAX_SAFE_INTEGER, role: "visitor", body: text };
      state.m.push(pending);
      draw();
      var visitor = "";
      try {
        visitor = localStorage.getItem("splee_vid") || "";
      } catch (err) {}
      post("/messages", { conversation: state.c, token: state.t, text: text, page: location.pathname, visitor: visitor })
        .then(function (j) {
          state.m.splice(state.m.indexOf(pending), 1);
          if (j.token) state.t = j.token;
          state.c = j.conversation;
          if (j.ask_email) state.ask = true;
          busy = false;
          take(j.messages);
          poll();
        })
        .catch(function (err) {
          state.m.splice(state.m.indexOf(pending), 1);
          input.value = text;
          busy = false;
          error = err.message;
          draw();
        });
    };
    input.onkeydown = function (e) {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        form.requestSubmit();
      }
    };
    talk.onclick = function () {
      state.ask = true;
      save();
      draw();
    };
    button.onclick = function () {
      state.open = !state.open;
      state.unread = false;
      save();
      draw();
      if (state.open) input.focus();
      poll();
    };
    close.onclick = button.onclick;
    document.addEventListener("visibilitychange", poll);
    draw();
    poll();
  }
})();
