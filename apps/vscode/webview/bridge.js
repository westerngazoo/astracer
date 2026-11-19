// Livewalk VS Code bridge: installs `window.livewalkVsCode` before the wasm
// app loads. The frontend's `VsCodeTransport` (apps/desktop/frontend/src/
// transport.rs) calls `request(method, params)`; each call is posted to the
// extension with a fresh id and settled by the matching `response` message.
// `focus` messages (select the function at file:line) go to `onFocus`
// handlers; one that arrives before the app subscribes is held for it.
(function () {
  "use strict";
  var vscode = acquireVsCodeApi();
  var pending = new Map();
  var progressHandlers = [];
  var focusHandlers = [];
  var heldFocus = null;
  var nextId = 1;

  function meta(name) {
    var el = document.querySelector('meta[name="livewalk-initial-' + name + '"]');
    return el ? el.getAttribute("content") || "" : "";
  }

  function parseFocus(text) {
    if (!text) {
      return null;
    }
    try {
      var f = JSON.parse(text);
      return f && typeof f.file === "string"
        ? { file: f.file, line: typeof f.line === "number" ? f.line : null }
        : null;
    } catch (e) {
      return null;
    }
  }

  window.addEventListener("message", function (event) {
    var msg = event.data;
    if (!msg || typeof msg !== "object") {
      return;
    }
    if (msg.type === "response") {
      var p = pending.get(msg.id);
      if (!p) {
        return;
      }
      pending.delete(msg.id);
      if (msg.ok) {
        try {
          p.resolve(msg.resultJson !== undefined ? JSON.parse(msg.resultJson) : msg.result);
        } catch (e) {
          p.reject("decoding response: " + e);
        }
      } else {
        p.reject(String(msg.error));
      }
    } else if (msg.type === "progress") {
      progressHandlers.forEach(function (h) {
        h(msg.payload);
      });
    } else if (msg.type === "focus" && typeof msg.file === "string") {
      var focus = { file: msg.file, line: typeof msg.line === "number" ? msg.line : null };
      if (focusHandlers.length === 0) {
        heldFocus = focus;
      }
      focusHandlers.forEach(function (h) {
        h(focus);
      });
    }
  });

  window.livewalkVsCode = {
    initialRepo: meta("repo"),
    initialLanguage: meta("language"),
    initialCached: meta("cached") === "1",
    initialFocus: parseFocus(meta("focus")),
    request: function (method, params) {
      var id = nextId++;
      return new Promise(function (resolve, reject) {
        pending.set(id, { resolve: resolve, reject: reject });
        vscode.postMessage({ type: "request", id: id, method: method, params: params });
      });
    },
    onProgress: function (handler) {
      progressHandlers.push(handler);
    },
    onFocus: function (handler) {
      focusHandlers.push(handler);
      if (heldFocus) {
        var f = heldFocus;
        heldFocus = null;
        handler(f);
      }
    },
  };
})();
