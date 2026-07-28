/* bridge.js — pywebview-shaped shim over Tauri.
   The original app ran under pywebview: app.js/pill.html call
   `window.pywebview.api.*` and the backend pushes via evaluate_js into global
   functions (tiroApplyState, pillSet, ...). This shim recreates the api
   surface on top of Tauri's `invoke`; pushes come in via WebviewWindow::eval
   from Rust, mirroring evaluate_js. Loaded before app.js in index.html (and
   in pill.html). */
(function () {
  "use strict";

  var TAURI = window.__TAURI__;
  if (!TAURI) {
    // Standalone browser preview (no Tauri): leave window.pywebview undefined
    // so app.js falls back to its built-in mock, same as the original.
    return;
  }
  var invoke = TAURI.core.invoke;

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
      rebind_shortcut: function (which, combo) { return invoke("rebind_shortcut", { which: which, combo: combo }); },
      list_models: function () { return invoke("list_models"); },
      download_model: function (name) { return invoke("download_model", { name: name }); }
    }
  };

  /* Backend -> JS pushes arrive exactly like pywebview's evaluate_js: the
     Rust side calls window.tiroApplyState(...)/window.pillSet(...)/etc.
     directly via WebviewWindow::eval, so no event plumbing is needed here. */

  /* pywebview fires this once the bridge is live; app.js boots on it. */
  window.dispatchEvent(new Event("pywebviewready"));
})();
