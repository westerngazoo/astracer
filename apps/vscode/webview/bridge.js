// Livewalk VS Code bridge: installs `window.livewalkVsCode` before the wasm
// app loads. The frontend's `VsCodeTransport` (apps/desktop/frontend/src/
// transport.rs) calls `request(method, params)`; each call is posted to the
// extension with a fresh id and settled by the matching `response` message.
(function () {
  "use strict";
  var vscode = acquireVsCodeApi();
  var pending = new Map();
  var progressHandlers = [];
  var nextId = 1;

  var meta = document.querySelector('meta[name="livewalk-initial-repo"]');

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
    }
  });

  window.livewalkVsCode = {
    initialRepo: meta ? meta.getAttribute("content") || "" : "",
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
  };
})();
