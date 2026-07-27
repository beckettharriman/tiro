/* bridge.js — pywebview-shaped shim over Tauri.
   The original app ran under pywebview: app.js/pill.html call
   `window.pywebview.api.*` and the backend pushes via evaluate_js into global
   functions (tiroApplyState, pillSet, ...). This shim recreates that exact
   surface on top of Tauri's `invoke` + event system so the original UI files
   stay verbatim. Loaded before app.js in index.html (and in pill.html). */
(function () {
  "use strict";

  var TAURI = window.__TAURI__;
  if (!TAURI) {
    // Standalone browser preview (no Tauri): leave window.pywebview undefined
    // so app.js falls back to its built-in mock, same as the original.
    return;
  }
  var invoke = TAURI.core.invoke;
  var listen = TAURI.event.listen;

  /* ── JS -> backend: every pywebview api method, same names & signatures ── */
  window.pywebview = {
    api: {
      get_state: function () { return invoke("get_state"); },
      copy_text: function (text) { return invoke("copy_text", { text: text }); },
      set_setting: function (key, value) { return invoke("set_setting", { key: key, value: value }); },
      list_mics: function () { return invoke("list_mics"); },
      toggle_record: function () { return invoke("toggle_record"); },
      cancel_record: function () { return invoke("cancel_record"); },
      set_pin: function (on) { return invoke("set_pin", { on: on }); },
      close_panel: function () { return invoke("close_panel"); },
      begin_drag: function () { return invoke("begin_drag"); },
      pick_folder: function () { return invoke("pick_folder"); },
      rebind_shortcut: function (which, combo) { return invoke("rebind_shortcut", { which: which, combo: combo }); }
    }
  };

  /* ── backend -> JS: Tauri events forwarded to the original global functions.
     The globals are resolved at event time (app.js / pill.html define them
     after this script runs). ── */
  var PUSHES = {
    "tiro://state":     function (p) { if (window.tiroApplyState) window.tiroApplyState(p); },
    "tiro://entry":     function (p) { if (window.tiroAddEntry) window.tiroAddEntry(p); },
    "tiro://engine":    function (p) { if (window.tiroSetEngine) window.tiroSetEngine(p); },
    "tiro://theme":     function (p) { if (window.tiroSetTheme) window.tiroSetTheme(p); },
    "tiro://recording": function (p) { if (window.tiroSetRecording) window.tiroSetRecording(p); },
    "tiro://storage":   function (p) { if (window.tiroSetStorage) window.tiroSetStorage(p); },
    // pill payload: { state: "recording"|..., payload?: string }
    "tiro://pill":      function (p) {
      if (window.pillSet) window.pillSet(p && p.state, p && p.payload);
    }
  };
  Object.keys(PUSHES).forEach(function (name) {
    listen(name, function (event) { PUSHES[name](event.payload); });
  });

  /* pywebview fires this once the bridge is live; app.js boots on it. */
  window.dispatchEvent(new Event("pywebviewready"));
})();
