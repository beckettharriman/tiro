/* app.js — TIRO panel + settings (OFFLINE vanilla rewrite of
   app.jsx + components.jsx + settings.jsx). No React, no Babel, no CDN. */
(function () {
  "use strict";

  /* ════════════════════════════════════════════════════════════════════
     ICONS — ported verbatim from components.jsx (currentColor line glyphs)
     Each returns an SVG element. Optional extra class via `cls`.
     ════════════════════════════════════════════════════════════════════ */
  function svg(markup, cls) {
    const wrap = document.createElement("div");
    wrap.innerHTML = markup.trim();
    const node = wrap.firstElementChild;
    if (cls) node.setAttribute("class", cls);
    return node;
  }
  const Icon = {
    Mic: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">' +
      '<rect x="9" y="3" width="6" height="11" rx="3"/>' +
      '<path d="M5.5 11a6.5 6.5 0 0 0 13 0"/>' +
      '<path d="M12 17.5V21"/></svg>', cls),
    Stop: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="currentColor">' +
      '<rect x="7" y="7" width="10" height="10" rx="3"/></svg>', cls),
    Pin: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M9 4h6l-.6 4.2 2.6 2.6V13H7v-2.2l2.6-2.6L9 4Z"/>' +
      '<path d="M12 13v7"/></svg>', cls),
    Gear: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.55" stroke-linecap="round" stroke-linejoin="round">' +
      '<circle cx="12" cy="12" r="3"/>' +
      '<path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z"/></svg>', cls),
    Close: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round">' +
      '<path d="M6.5 6.5l11 11M17.5 6.5l-11 11"/></svg>', cls),
    ChevronD: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M8 10l4 4 4-4"/></svg>', cls),
    UpDown: (cls) => svg(
      '<svg viewBox="0 0 12 16" fill="currentColor">' +
      '<path d="M6 1 9 4.6H3zM6 15 3 11.4h6z"/></svg>', cls),
    Back: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.1" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M14.5 6.5 9 12l5.5 5.5"/></svg>', cls),
    Check: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M5 12.5l4.2 4.2L19 7"/></svg>', cls),
    CheckCircle: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">' +
      '<circle cx="12" cy="12" r="9"/>' +
      '<path d="M8 12.4l2.6 2.6L16 9.4"/></svg>', cls),
    Bolt: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="currentColor">' +
      '<path d="M13 2 4.6 13.1c-.3.4 0 1 .5 1H10l-1 7.9c-.1.6.7.9 1 .4L18.4 11c.3-.4 0-1-.5-1H14l1-7.6c.1-.6-.6-.9-1-.4z"/></svg>', cls),
    Battery: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.6">' +
      '<rect x="2.5" y="8" width="16" height="9" rx="2.6"/>' +
      '<rect x="4.3" y="9.8" width="8" height="5.4" rx="1.2" fill="currentColor" stroke="none"/>' +
      '<path d="M21 11v3" stroke-linecap="round"/></svg>', cls),
    Vol: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7" stroke-linecap="round" stroke-linejoin="round">' +
      '<path d="M4 9v6h3l5 4V5L7 9H4Z"/>' +
      '<path d="M16 9.5a3.5 3.5 0 0 1 0 5"/></svg>', cls),
    Transparency: (cls) => svg(
      '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.7">' +
      '<circle cx="12" cy="12" r="8.5"/>' +
      '<path d="M12 3.5a8.5 8.5 0 0 0 0 17z" fill="currentColor" stroke="none"/></svg>', cls)
  };

  /* ════════════════════════════════════════════════════════════════════
     SMALL DOM HELPERS
     ════════════════════════════════════════════════════════════════════ */
  function h(tag, attrs, children) {
    const n = document.createElement(tag);
    if (attrs) {
      for (const k in attrs) {
        const v = attrs[k];
        if (v == null) continue;
        if (k === "class") n.className = v;
        else if (k === "text") n.textContent = v;
        else if (k === "html") n.innerHTML = v;
        else if (k.slice(0, 2) === "on" && typeof v === "function") {
          n.addEventListener(k.slice(2).toLowerCase(), v);
        } else if (k === "style" && typeof v === "object") {
          Object.assign(n.style, v);
        } else {
          n.setAttribute(k, v);
        }
      }
    }
    if (children != null) {
      const arr = Array.isArray(children) ? children : [children];
      arr.forEach((c) => {
        if (c == null || c === false) return;
        n.appendChild(typeof c === "string" || typeof c === "number"
          ? document.createTextNode(String(c)) : c);
      });
    }
    return n;
  }

  /* ── shortcut helpers (ported from app.jsx) ──────────────────────────── */
  function keyLabel(code) {
    if (code.startsWith("Key")) return code.slice(3);
    if (code.startsWith("Digit")) return code.slice(5);
    const m = { Space: "Space", Enter: "Enter", Tab: "Tab", Escape: "Esc", Backquote: "`",
      ArrowUp: "↑", ArrowDown: "↓", ArrowLeft: "←", ArrowRight: "→",
      Minus: "−", Equal: "=", Backslash: "\\", Slash: "/", Period: ".", Comma: "," };
    return m[code] || code;
  }
  function comboKeys(e) {
    const a = [];
    if (e.ctrlKey) a.push("Ctrl");
    if (e.altKey) a.push("Alt");
    if (e.shiftKey) a.push("Shift");
    if (e.metaKey) a.push("Win");
    a.push(keyLabel(e.code));
    return a;
  }

  /* ════════════════════════════════════════════════════════════════════
     API — real pywebview bridge, or a MOCK fallback for standalone preview
     ════════════════════════════════════════════════════════════════════ */
  const HAS_BRIDGE = !!(window.pywebview && window.pywebview.api);

  const MOCK_STATE = {
    entries: [],
    settings: {
      powerMode: "auto", modelBattery: "base.en", modelPlugged: "small.en",
      soundCues: true, volume: 65, recordingPill: true,
      clipboardCleanup: "light", smartVocab: true,
      micName: "", launchAtLogin: true,
      saveTranscripts: true, savePath: "", transparency: 45,
      storageFallback: false, storagePath: ""
    },
    engine: { model: "small.en", device: "GPU", power: "plugged" },
    mics: [],
    shortcuts: {
      dictate: { ctrl: true, alt: true, shift: false, meta: false, code: "Space", keys: ["Ctrl", "Alt", "Space"] },
      panel:   { ctrl: true, alt: true, shift: false, meta: false, code: "KeyV", keys: ["Ctrl", "Alt", "V"] },
      cancel:  { ctrl: true, alt: true, shift: false, meta: false, code: "KeyX", keys: ["Ctrl", "Alt", "X"] }
    },
    theme: "dark", effectiveTheme: "dark",
    // list_models() shape (mock preview only; the real list comes from Rust)
    models: [
      { name: "tiny",           hint: "Fastest — very low accuracy, all languages", curated: false, sizeBytes: 43537433,   installed: false, downloading: false },
      { name: "tiny.en",        hint: "Fastest — very low accuracy",                curated: true,  sizeBytes: 43550795,   installed: false, downloading: false },
      { name: "base",           hint: "Fast — all languages",                       curated: false, sizeBytes: 81768585,   installed: false, downloading: false },
      { name: "base.en",        hint: "Fast — battery default",                     curated: true,  sizeBytes: 81781811,   installed: true,  downloading: false },
      { name: "small",          hint: "Balanced — all languages",                   curated: false, sizeBytes: 264464607,  installed: false, downloading: false },
      { name: "small.en",       hint: "Balanced — plugged default",                 curated: true,  sizeBytes: 264477561,  installed: true,  downloading: false },
      { name: "medium",         hint: "Accurate — slower on CPU, all languages",    curated: false, sizeBytes: 823369779,  installed: false, downloading: false },
      { name: "medium.en",      hint: "Accurate — slower on CPU",                   curated: true,  sizeBytes: 823382461,  installed: false, downloading: false },
      { name: "large-v1",       hint: "Original large — all languages",             curated: false, sizeBytes: 3094623691, installed: false, downloading: false },
      { name: "large-v2",       hint: "Very accurate — all languages",              curated: false, sizeBytes: 1656129691, installed: false, downloading: false },
      { name: "large-v3",       hint: "Very accurate — all languages",              curated: false, sizeBytes: 3095033483, installed: false, downloading: false },
      { name: "large-v3-turbo", hint: "Most accurate — GPU recommended, all languages", curated: true, sizeBytes: 874188075, installed: false, downloading: false }
    ]
  };

  function clone(o) { return JSON.parse(JSON.stringify(o)); }

  const MockApi = {
    _state: clone(MOCK_STATE),
    _deriveEngine() {
      const s = this._state.settings;
      const power = this._state.engine.power;
      const device = s.powerMode === "auto"
        ? (power === "plugged" ? "GPU" : "CPU")
        : (s.powerMode === "gpu" ? "GPU" : "CPU");
      const model = power === "plugged" ? s.modelPlugged : s.modelBattery;
      this._state.engine = { model, device, power };
      return this._state.engine;
    },
    get_state() { this._deriveEngine(); return Promise.resolve(clone(this._state)); },
    copy_text() { return Promise.resolve(null); },
    set_setting(key, value) {
      if (key === "theme") {
        this._state.theme = value;
        this._state.effectiveTheme = value === "system" ? "dark" : value;
      } else {
        this._state.settings[key] = value;
      }
      const engine = this._deriveEngine();
      return Promise.resolve({
        ok: true, engine: clone(engine),
        theme: this._state.theme, effectiveTheme: this._state.effectiveTheme,
        launchAtLogin: this._state.settings.launchAtLogin
      });
    },
    list_mics() { return Promise.resolve(clone(this._state.mics)); },
    toggle_record() { return Promise.resolve(null); },
    cancel_record() { return Promise.resolve(null); },
    set_pin() { return Promise.resolve(null); },
    close_panel() { return Promise.resolve(null); },
    begin_drag() { return Promise.resolve(null); },
    pick_folder() {
      // Standalone preview has no real folder picker — resolve null so the
      // panel keeps whatever (empty) savePath it already has.
      return Promise.resolve(null);
    },
    rebind_shortcut(which, combo) {
      this._state.shortcuts[which] = combo;
      return Promise.resolve({ ok: true, keys: combo.keys });
    },
    list_models() { return Promise.resolve(clone(this._state.models)); },
    download_model(name) {
      // Simulate a download: ~1.6 s of 10% progress pushes, then done.
      const m = this._state.models.find((x) => x.name === name);
      if (!m || m.installed) return Promise.resolve({ ok: true, installed: true });
      let pct = 0;
      const tick = () => {
        pct += 10;
        if (pct >= 100) {
          m.installed = true;
          m.installedBytes = m.sizeBytes;
          if (window.tiroModelProgress) window.tiroModelProgress({ model: name, pct: 100, done: true, error: null });
        } else {
          if (window.tiroModelProgress) window.tiroModelProgress({ model: name, pct: pct, done: false, error: null });
          setTimeout(tick, 160);
        }
      };
      setTimeout(tick, 160);
      return Promise.resolve({ ok: true, started: true });
    }
  };

  // `api` is mutable so a late-arriving pywebview bridge can replace the mock.
  let api = HAS_BRIDGE ? window.pywebview.api : MockApi;

  /* ════════════════════════════════════════════════════════════════════
     APP STATE
     ════════════════════════════════════════════════════════════════════ */
  const App = {
    entries: [],
    settings: {},
    engine: { model: "", device: "CPU", power: "battery" },
    mics: [],
    shortcuts: {},
    theme: "dark",
    recording: false,
    pinned: false,
    view: "panel",        // panel | settings
    models: [],           // list_models() snapshot (model manager)
    modelsExpanded: false, // "Show all models" fold state
    modelProgress: {},    // model name -> latest tiroModelProgress payload
    copiedId: null,
    expandedIds: new Set(),  // entry ids whose transcript fold is expanded
    editing: null,        // shortcut key being captured: dictate | panel | cancel
    shortcutHint: null,   // transient "couldn't change shortcut" hint, or null
    _copyTimer: null,
    _captureHandler: null
  };

  const els = {
    panelPage: document.getElementById("panelPage"),
    settingsPage: document.getElementById("settingsPage")
  };

  /* ════════════════════════════════════════════════════════════════════
     ENGINE CHIP + small reusable pieces
     ════════════════════════════════════════════════════════════════════ */
  function powerWord(power) { return power === "plugged" ? "plugged in" : "battery"; }

  function engineChip(engine) {
    return h("span", { class: "chip", title: "Transcription engine — switches with power source" }, [
      engine.power === "plugged" ? Icon.Bolt() : Icon.Battery(),
      h("span", { text: engine.model }),
      h("span", { text: "·" }),
      h("span", { class: "dev", text: engine.device }),
      h("span", { class: "pwr", text: "(" + powerWord(engine.power) + ")" })
    ]);
  }

  /* ════════════════════════════════════════════════════════════════════
     PANEL PAGE
     ════════════════════════════════════════════════════════════════════ */
  function entryNode(e) {
    const time = h("div", { class: "entry-time" }, [
      h("span", { text: e.clock }),
      h("span", { class: "entry-dur", text: "· " + e.dur })
    ]);
    if (App.copiedId === e.id) {
      time.appendChild(h("span", { class: "copied-chip" }, [Icon.Check(), "Copied"]));
    }
    // Long transcripts clamp to a few lines by default; a distinct "Show more"
    // control toggles the fold. Track expanded-ids in App so it survives re-render.
    const expanded = App.expandedIds.has(e.id);
    const textEl = h("div", {
      class: "entry-text clamped" + (expanded ? " expanded" : ""),
      text: e.text
    });
    const expandBtn = h("button", {
      type: "button", class: "entry-expand",
      "aria-expanded": String(expanded),
      text: expanded ? "Show less" : "Show more",
      // Distinct control — stop propagation so it doesn't also copy the entry.
      onmousedown: (ev) => ev.stopPropagation(),
      onclick: (ev) => { ev.stopPropagation(); toggleExpand(e.id); },
      onkeydown: (ev) => {
        if (ev.key === "Enter" || ev.key === " ") { ev.stopPropagation(); }
      }
    });
    const node = h("div", {
      class: "entry" + (App.copiedId === e.id ? " copied" : "") + (e.fresh ? " fresh" : ""),
      "data-id": e.id,
      role: "button", tabindex: "0",
      // Prevent focus-on-mousedown: focusing a partially-visible entry makes the
      // browser scroll it into view, which is the "it scrolled me around" jitter.
      // The click (copy) still fires; keyboard users still Tab + Enter/Space.
      onmousedown: (ev) => ev.preventDefault(),
      onclick: () => copyEntry(e.id, e.text),
      onkeydown: (ev) => {
        if (ev.key === "Enter" || ev.key === " ") { ev.preventDefault(); copyEntry(e.id, e.text); }
      }
    }, [
      time,
      textEl,
      expandBtn
    ]);
    return node;
  }

  // The "Show more" control is only useful when the text actually overflows its
  // clamp. CSS can't measure overflow, so flag overflowing entries with a class
  // (CSS hides .entry-expand unless .has-overflow is present). Run after layout.
  function markOverflow(scope) {
    const root = scope || els.panelPage;
    const texts = root.querySelectorAll(".entry-text.clamped:not(.expanded)");
    texts.forEach((t) => {
      const entry = t.closest(".entry");
      if (!entry) return;
      // scrollHeight exceeds clientHeight by ~>1px when content is clipped.
      const overflow = t.scrollHeight - t.clientHeight > 1;
      entry.classList.toggle("has-overflow", overflow);
    });
  }
  function scheduleOverflowCheck(scope) {
    // Defer to after the browser has laid the (just-inserted) nodes out.
    requestAnimationFrame(() => markOverflow(scope));
  }

  function toggleExpand(id) {
    if (App.expandedIds.has(id)) App.expandedIds.delete(id);
    else App.expandedIds.add(id);
    // Update IN PLACE — a full renderPanel() would reset the list scroll position.
    const node = els.panelPage.querySelector('.entry[data-id="' + id + '"]');
    if (!node) return;
    const expanded = App.expandedIds.has(id);
    const textEl = node.querySelector(".entry-text");
    const btn = node.querySelector(".entry-expand");
    if (textEl) textEl.classList.toggle("expanded", expanded);
    if (btn) {
      btn.setAttribute("aria-expanded", String(expanded));
      btn.textContent = expanded ? "Show less" : "Show more";
    }
    // Re-check overflow when collapsing (re-clamped text may overflow again).
    if (!expanded) scheduleOverflowCheck(node);
  }

  function micControl() {
    // A native <select> overlaid invisibly on the styled mic pill so the
    // choice actually works inside pywebview (no React state needed).
    const sel = h("select", {
      class: "mic-native", title: "Choose input device",
      onchange: (ev) => onMicChange(ev.target.value)
    }, App.mics.map((m) =>
      h("option", { value: m, selected: m === App.settings.micName ? "selected" : null, text: m })
    ));
    const pill = h("span", { class: "mic" }, [
      h("span", { class: "mic-led" }),
      h("span", { class: "mic-name", text: App.settings.micName || "Default microphone" }),
      Icon.ChevronD()
    ]);
    return h("span", { class: "mic-shell" }, [pill, sel]);
  }

  function renderPanel() {
    const page = els.panelPage;
    page.innerHTML = "";
    page.setAttribute("aria-hidden", App.view === "settings" ? "true" : "false");
    page.style.left = App.view === "settings" ? "-100%" : "0";

    /* header — drag handle (mousedown anywhere but a button starts a window drag) */
    const header = h("div", { class: "hd", onmousedown: beginDrag }, [
      h("div", { class: "hd-title", text: "Recent transcriptions:" }),
      h("div", { class: "hd-actions" }, [
        h("button", {
          class: "ico pin", "data-on": App.pinned ? "1" : "0", "aria-pressed": String(App.pinned),
          title: App.pinned ? "Pinned on top — click to unpin" : "Keep on top", onclick: onPin
        }, [Icon.Pin()]),
        h("button", { class: "ico", title: "Close", onclick: onClose }, [Icon.Close()])
      ])
    ]);
    page.appendChild(header);

    /* mic selector + record */
    page.appendChild(h("div", { class: "mic-wrap" }, [
      micControl(),
      h("button", {
        class: "ico rec", "data-on": App.recording ? "1" : "0",
        "aria-pressed": String(App.recording),
        title: App.recording ? "Stop recording" : "Start recording",
        onclick: onRecord
      }, [App.recording ? Icon.Stop() : Icon.Mic()])
    ]));

    /* transcript list or empty state */
    if (!App.entries.length) {
      page.appendChild(h("div", { class: "empty" }, [
        h("div", { class: "empty-kbd" },
          (App.shortcuts.dictate ? App.shortcuts.dictate.keys : ["Ctrl", "Alt", "Space"])
            .map((k) => h("span", { class: "kbd", text: k }))),
        h("div", { class: "empty-msg",
          html: "Press the hotkey anywhere to start dictating.<br>Your words land here, ready to copy." })
      ]));
    } else {
      const inner = h("div", { class: "list-inner" }, App.entries.map(entryNode));
      page.appendChild(h("div", { class: "list" }, [inner]));
    }

    /* footer — live engine chip + settings gear */
    page.appendChild(h("div", { class: "ft" }, [
      engineChip(App.engine),
      h("button", { class: "ico sm", title: "Settings", onclick: onGear }, [Icon.Gear()])
    ]));

    /* Flag which (re-rendered) entries actually overflow their clamp so the
       "Show more" control only appears where it's needed (tiroApplyState +
       tiroAddEntry both flow through here). */
    if (App.entries.length) scheduleOverflowCheck();
  }

  /* ════════════════════════════════════════════════════════════════════
     SETTINGS CONTROLS
     ════════════════════════════════════════════════════════════════════ */
  function switchControl(on, onChange) {
    return h("button", {
      type: "button", class: "sw", "data-on": on ? "1" : "0",
      role: "switch", "aria-checked": String(!!on),
      onclick: () => onChange(!on)
    }, [h("i")]);
  }

  function segmented(value, options, onChange) {
    const opts = options.map((o) => (typeof o === "object" ? o : { value: o, label: o }));
    const idx = Math.max(0, opts.findIndex((o) => o.value === value));
    const n = opts.length;
    const thumb = h("div", { class: "segm-thumb", style: {
      left: "calc(2px + " + idx + " * (100% - 4px) / " + n + ")",
      width: "calc((100% - 4px) / " + n + ")"
    }});
    const buttons = opts.map((o) => h("button", {
      type: "button", role: "radio", "aria-checked": String(o.value === value),
      "data-on": o.value === value ? "1" : "0",
      onclick: () => onChange(o.value), text: o.label
    }));
    return h("div", { class: "segm", role: "radiogroup" }, [thumb].concat(buttons));
  }

  function slider(value, min, max, onChange, icon, onInput) {
    const pct = ((value - min) / (max - min)) * 100;
    const fill = "linear-gradient(to right, var(--accent) 0 " + pct + "%, var(--fill-2) " + pct + "% 100%)";
    const input = h("input", {
      type: "range", class: "sldr", min: String(min), max: String(max), value: String(value),
      style: { background: fill },
      oninput: (ev) => {
        const v = Number(ev.target.value);
        const p = ((v - min) / (max - min)) * 100;
        ev.target.style.background =
          "linear-gradient(to right, var(--accent) 0 " + p + "%, var(--fill-2) " + p + "% 100%)";
        if (onInput) onInput(v);            // live preview while dragging
      },
      onchange: (ev) => onChange(Number(ev.target.value))
    });
    return h("div", { class: "sldr-wrap" }, [icon, input]);
  }

  function selectControl(value, options, onChange) {
    // title = current value so a name truncated by the ellipsis is still
    // readable on hover (the native dropdown always shows full names).
    const sel = h("select", {
      class: "sel", title: value || null,
      onchange: (ev) => { ev.target.title = ev.target.value; onChange(ev.target.value); }
    }, options.map((o) => h("option", { value: o, selected: o === value ? "selected" : null, text: o })));
    return h("span", { class: "sel-wrap" }, [sel, Icon.UpDown("sel-chev")]);
  }

  function shortcutRow(label, which) {
    const combo = App.shortcuts[which] || { keys: [] };
    let keysEl;
    if (App.editing === which) {
      keysEl = h("span", { class: "keys listening" }, [
        h("span", { class: "keys-listen-lbl", text: "Press keys…" }),
        h("button", { class: "keys-edit", text: "Cancel", onclick: cancelEdit })
      ]);
    } else {
      keysEl = h("span", { class: "keys" },
        combo.keys.map((k) => h("span", { class: "kbd2", text: k }))
          .concat([h("button", { class: "keys-edit", text: "Edit", onclick: () => startEdit(which) })]));
    }
    return h("div", { class: "row" }, [
      h("div", { class: "row-l" }, [h("span", { class: "row-label", text: label })]),
      keysEl
    ]);
  }

  function row(label, sub, control) {
    const left = h("div", { class: "row-l" }, [h("span", { class: "row-label", text: label })]);
    if (sub != null) {
      if (typeof sub === "string") left.appendChild(h("span", { class: "row-sub", text: sub }));
      else left.appendChild(sub);
    }
    return h("div", { class: "row" }, [left, control]);
  }

  function rowCol(label, control) {
    return h("div", { class: "row col" }, [
      h("span", { class: "row-label", text: label }),
      control
    ]);
  }

  function group(title, rows, helper) {
    const node = h("div", { class: "grp" }, [
      h("div", { class: "grp-h", text: title }),
      h("div", { class: "card" }, rows)
    ]);
    if (helper) node.appendChild(h("div", { class: "grp-helper", text: helper }));
    return node;
  }

  // Calm inline banner shown inside the Storage card when writes fell back to a
  // safe folder because the chosen vault dir was unwritable. Plain text only
  // (never innerHTML), existing row/separator tokens, no shadow.
  function storageBanner(path) {
    return h("div", { class: "row storage-fallback" }, [
      h("span", { class: "row-sub",
        text: "Saving to fallback folder — " + (path || "chosen folder") + " unavailable" })
    ]);
  }

  /* ── model manager (Models group + selector options) ─────────────────── */
  function fmtSize(bytes) {
    if (!bytes) return "";
    if (bytes >= 1e9) return (bytes / 1e9).toFixed(1) + " GB";
    return Math.round(bytes / 1e6) + " MB";
  }

  // Battery/plugged selector options: installed models (catalog order) plus
  // the currently-configured values even when not installed.
  function modelOptions() {
    const opts = App.models.filter((m) => m.installed).map((m) => m.name);
    [App.settings.modelBattery, App.settings.modelPlugged].forEach((v) => {
      if (v && opts.indexOf(v) < 0) opts.push(v);
    });
    if (!opts.length) opts.push("base.en");
    return opts;
  }

  function refreshModels() {
    Promise.resolve(api.list_models && api.list_models()).then((list) => {
      if (!Array.isArray(list)) return;
      App.models = list;
      renderSettings();
    }).catch(() => {});
  }

  function startDownload(name) {
    App.modelProgress[name] = { model: name, pct: 0, done: false, error: null };
    updateModelRow(name);
    Promise.resolve(api.download_model(name)).then((res) => {
      if (res && res.ok === false) {
        App.modelProgress[name] = { model: name, pct: 0, done: false, error: res.error || "Download failed" };
        updateModelRow(name);
      } else if (res && res.installed) {
        // already on disk (race with another path) — no pushes will come
        delete App.modelProgress[name];
        refreshModels();
      }
    }).catch(() => {
      App.modelProgress[name] = { model: name, pct: 0, done: false, error: "Download failed" };
      updateModelRow(name);
    });
  }

  // The right-hand state cell of one model row: ✓ installed, Download,
  // live percent, or error + Retry.
  function modelStateEl(m) {
    const p = App.modelProgress[m.name];
    if (p && p.error) {
      return h("span", { class: "model-state" }, [
        h("span", { class: "model-err", title: p.error, text: "Failed" }),
        h("button", { class: "btn-mini", text: "Retry", onclick: () => startDownload(m.name) })
      ]);
    }
    if (p && !p.done) {
      return h("span", { class: "model-state pct", text: p.pct + "%" });
    }
    if (m.installed) {
      return h("span", { class: "model-state ok", title: "Installed" }, [Icon.Check(), "Installed"]);
    }
    if (m.downloading) {
      return h("span", { class: "model-state pct", text: "…" });
    }
    return h("button", { class: "btn-mini", text: "Download", onclick: () => startDownload(m.name) });
  }

  function modelRow(m) {
    const sub = m.hint + (m.sizeBytes ? " · " + fmtSize(m.sizeBytes) : "");
    const node = row(m.name, sub, h("span", { class: "model-slot" }, [modelStateEl(m)]));
    node.setAttribute("data-model", m.name);
    return node;
  }

  // Update one row's state cell in place — a full renderSettings() per 1%
  // progress tick would tear down open dropdowns and reset scroll.
  function updateModelRow(name) {
    const slot = els.settingsPage.querySelector('.row[data-model="' + name + '"] .model-slot');
    const m = App.models.find((x) => x.name === name);
    if (!slot || !m) return;
    slot.innerHTML = "";
    slot.appendChild(modelStateEl(m));
  }

  function renderSettings() {
    const page = els.settingsPage;
    // Rebuilding the DOM resets the body's scroll position — remember it so a
    // re-render (e.g. after a set_setting ack) doesn't jump back to the top.
    const prevBody = page.querySelector(".set-body");
    const keepScroll = prevBody ? prevBody.scrollTop : 0;
    page.innerHTML = "";
    page.setAttribute("aria-hidden", App.view === "panel" ? "true" : "false");
    page.style.left = App.view === "settings" ? "0" : "100%";

    const s = App.settings;
    const engine = App.engine;

    /* header */
    page.appendChild(h("div", { class: "set-hd" }, [
      h("button", { class: "back", title: "Back to Tiro", onclick: onBack }, [Icon.Back(), "Tiro"]),
      h("span", { class: "set-title", text: "Settings" })
    ]));

    const body = h("div", { class: "set-body" });

    /* ENGINE */
    const nowRow = h("div", { class: "now-row" }, [
      h("span", { class: "now-ico" }, [engine.power === "plugged" ? Icon.Bolt() : Icon.Battery()]),
      h("span", { class: "now-txt" }, [
        h("span", { class: "now-lab", text: "Now running" }),
        h("span", { class: "now-val" }, [
          engine.model + " · ",
          h("b", { text: engine.device }),
          " · " + powerWord(engine.power)
        ])
      ])
    ]);
    body.appendChild(group("Engine", [
      nowRow,
      rowCol("Power mode", segmented(s.powerMode, [
        { value: "auto", label: "Auto" },
        { value: "cpu", label: "Always CPU" },
        { value: "gpu", label: "Always GPU" }
      ], (v) => setSetting("powerMode", v))),
      row("Model on battery", null, selectControl(s.modelBattery, modelOptions(), (v) => setSetting("modelBattery", v))),
      row("Model when plugged in", null, selectControl(s.modelPlugged, modelOptions(), (v) => setSetting("modelPlugged", v)))
    ], "Auto uses the GPU for accuracy when plugged in, and a lighter CPU model on battery to save power."));

    /* MODELS — curated rows always visible; "Show all models" reveals the rest */
    const curated = App.models.filter((m) => m.curated);
    const extras = App.models.filter((m) => !m.curated);
    const modelRows = curated.map(modelRow);
    if (App.modelsExpanded) extras.forEach((m) => modelRows.push(modelRow(m)));
    if (extras.length) {
      modelRows.push(h("div", { class: "row models-toggle" }, [
        h("button", {
          type: "button", class: "models-more",
          "aria-expanded": String(App.modelsExpanded),
          text: App.modelsExpanded ? "Hide extra models" : "Show all models",
          onclick: () => { App.modelsExpanded = !App.modelsExpanded; renderSettings(); }
        })
      ]));
    }
    if (modelRows.length) {
      body.appendChild(group("Models", modelRows,
        "Models download once and run fully offline. Downloaded models appear in the selectors above."));
    }

    /* AUDIO */
    body.appendChild(group("Audio", [
      row("Sound cues", null, switchControl(s.soundCues, (v) => setSetting("soundCues", v))),
      rowCol("Volume", slider(s.volume, 0, 100, (v) => setSetting("volume", v), Icon.Vol()))
    ]));

    /* CAPTURE */
    const captureHelper = s.clipboardCleanup === "fillers"
      ? "“+ Fillers” also strips um, uh, like, you know before copying."
      : "Light cleanup fixes spacing and capitalization on copy.";
    body.appendChild(group("Capture", [
      row("Recording pill", "Show the floating indicator while dictating",
        switchControl(s.recordingPill, (v) => setSetting("recordingPill", v))),
      rowCol("Clipboard cleanup", segmented(s.clipboardCleanup, [
        { value: "off", label: "Off" },
        { value: "light", label: "Light" },
        { value: "fillers", label: "+ Fillers" }
      ], (v) => setSetting("clipboardCleanup", v))),
      row("Smart vocabulary", "Better spelling of your names & slang",
        switchControl(s.smartVocab, (v) => setSetting("smartVocab", v)))
    ], captureHelper));

    /* INPUT */
    body.appendChild(group("Input", [
      row("Microphone", null,
        selectControl(s.micName, App.mics.length ? App.mics : [s.micName], (v) => setSetting("micName", v)))
    ]));

    /* SHORTCUTS */
    body.appendChild(group("Shortcuts", [
      shortcutRow("Dictate", "dictate"),
      shortcutRow("Open panel", "panel"),
      shortcutRow("Cancel recording", "cancel")
    ], App.shortcutHint || null));

    /* STORAGE */
    const saveOn = s.saveTranscripts !== false;
    const pathRow = row("Save transcripts to",
      h("span", { class: "path-val", text: s.savePath }),
      h("button", { class: "btn-mini", text: "Change…", onclick: onChangePath }));
    if (!saveOn) pathRow.classList.add("disabled");
    const storageRows = [
      row("Save transcripts", "Write each take to the folder below",
        switchControl(saveOn, (v) => setSetting("saveTranscripts", v))),
      pathRow
    ];
    // Calm inline fallback banner — only when the vault dir was unwritable.
    if (s.storageFallback) {
      storageRows.push(storageBanner(s.storagePath || s.savePath));
    }
    body.appendChild(group("Storage", storageRows));

    /* APPEARANCE (NEW group — immediately before System) */
    const transp = typeof s.transparency === "number" ? s.transparency : 45;
    body.appendChild(group("Appearance", [
      rowCol("Theme", segmented(App.theme, [
        { value: "light", label: "Light" },
        { value: "dark", label: "Dark" },
        { value: "system", label: "Follow system" }
      ], (v) => setSetting("theme", v))),
      rowCol("Transparency", slider(transp, 0, 100,
        (v) => persistTransparency(v),                 // commit on release
        Icon.Transparency(),
        (v) => { App.settings.transparency = v; applyTransparency(v); }))  // live preview
    ]));

    /* SYSTEM */
    body.appendChild(group("System", [
      row("Launch at login", null, switchControl(s.launchAtLogin, (v) => setSetting("launchAtLogin", v)))
    ]));

    page.appendChild(body);
    body.scrollTop = keepScroll;
  }

  /* ════════════════════════════════════════════════════════════════════
     ACTIONS / WIRING
     ════════════════════════════════════════════════════════════════════ */
  function applyEngine(engine) {
    if (!engine) return;
    App.engine = engine;
  }

  function applyTheme(effective) {
    document.documentElement.setAttribute("data-theme", effective === "light" ? "light" : "dark");
  }

  // Transparency slider (0 = solid, 100 = most see-through) -> panel alpha.
  // Maps to opacity 0.95 (t=0) .. 0.30 (t=100) so the panel never goes invisible.
  function applyTransparency(t) {
    const op = 0.95 - 0.65 * (Math.max(0, Math.min(100, t)) / 100);
    document.documentElement.style.setProperty("--panel-alpha", op.toFixed(3));
  }
  function persistTransparency(t) {
    App.settings.transparency = t;
    applyTransparency(t);
    Promise.resolve(api.set_setting("transparency", t)).catch(() => {});
  }

  function setSetting(key, value) {
    // optimistic local update, painted immediately (renderSettings keeps the
    // body's scroll position, so this never jumps the page)
    if (key === "theme") App.theme = value;
    else App.settings[key] = value;
    renderSettings();
    renderPanel();

    Promise.resolve(api.set_setting(key, value)).then((res) => {
      if (!res) return;
      // Re-render only when the ack actually changed something beyond the
      // optimistic update — not on every ack.
      let dirty = false;
      if (res.engine && (res.engine.model !== App.engine.model ||
          res.engine.device !== App.engine.device ||
          res.engine.power !== App.engine.power)) {
        applyEngine(res.engine);
        dirty = true;
      }
      if (res.theme != null && res.theme !== App.theme) { App.theme = res.theme; dirty = true; }
      if (res.effectiveTheme) applyTheme(res.effectiveTheme);
      if (res.launchAtLogin != null && res.launchAtLogin !== App.settings.launchAtLogin) {
        App.settings.launchAtLogin = res.launchAtLogin;
        dirty = true;
      }
      if (dirty) { renderSettings(); renderPanel(); }
    }).catch(() => { renderSettings(); renderPanel(); });
  }

  function onMicChange(name) {
    // setSetting repaints both pages, so no extra render needed here.
    setSetting("micName", name);
  }

  function clearCopiedMark() {
    const prev = els.panelPage.querySelector(".entry.copied");
    if (prev) {
      prev.classList.remove("copied");
      const chip = prev.querySelector(".copied-chip");
      if (chip) chip.remove();
    }
  }

  function copyEntry(id, text) {
    Promise.resolve(api.copy_text(text)).catch(() => {});
    clearTimeout(App._copyTimer);
    // Remember the scroll position up front so nothing can yank the list around.
    const list = els.panelPage.querySelector(".list");
    const keepTop = list ? list.scrollTop : 0;
    // Update the clicked entry IN PLACE — no full re-render, so the list's
    // scroll position is preserved (a renderPanel() here jumps it to the top).
    clearCopiedMark();
    App.copiedId = id;
    const node = els.panelPage.querySelector('.entry[data-id="' + id + '"]');
    if (node) {
      node.classList.add("copied");
      const timeRow = node.querySelector(".entry-time");
      if (timeRow && !timeRow.querySelector(".copied-chip")) {
        timeRow.appendChild(h("span", { class: "copied-chip" }, [Icon.Check(), "Copied"]));
      }
    }
    // Belt-and-suspenders: if focus/layout nudged the scroll, snap it right back
    // (now and on the next frame, since some scroll adjustments fire async).
    if (list) {
      list.scrollTop = keepTop;
      requestAnimationFrame(() => { list.scrollTop = keepTop; });
    }
    App._copyTimer = setTimeout(() => {
      App.copiedId = null;
      clearCopiedMark();
    }, 1500);
  }

  function onRecord() { Promise.resolve(api.toggle_record()).catch(() => {}); }

  function onPin() {
    App.pinned = !App.pinned;
    Promise.resolve(api.set_pin(App.pinned)).catch(() => {});
    // Update the pin button in place (don't renderPanel — that would reset scroll).
    const btn = els.panelPage.querySelector(".ico.pin");
    if (btn) {
      btn.setAttribute("data-on", App.pinned ? "1" : "0");
      btn.setAttribute("aria-pressed", String(App.pinned));
      btn.title = App.pinned ? "Pinned on top — click to unpin" : "Keep on top";
    }
  }

  function beginDrag(ev) {
    if (ev.button !== 0) return;                            // left button only
    if (ev.target && ev.target.closest("button")) return;  // header buttons still click
    ev.preventDefault();                                   // no text selection
    Promise.resolve(api.begin_drag && api.begin_drag()).catch(() => {});
  }

  function onClose() { Promise.resolve(api.close_panel()).catch(() => {}); }

  function onGear() { App.view = "settings"; renderPanel(); renderSettings(); }
  function onBack() { App.view = "panel"; renderPanel(); renderSettings(); }

  function onChangePath() {
    Promise.resolve(api.pick_folder()).then((res) => {
      if (res && res.path) {
        App.settings.savePath = res.path;
        renderSettings();
      }
    }).catch(() => {});
  }

  /* shortcut press-keys capture (ported from app.jsx) */
  function startEdit(which) {
    App.editing = which;
    renderSettings();
    const MODS = ["ControlLeft", "ControlRight", "AltLeft", "AltRight",
      "ShiftLeft", "ShiftRight", "MetaLeft", "MetaRight"];
    const cap = (e) => {
      if (MODS.includes(e.code)) return;
      e.preventDefault(); e.stopPropagation();
      if (e.code === "Escape") { cancelEdit(); return; }
      const combo = {
        ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey,
        code: e.code, keys: comboKeys(e)
      };
      teardownCapture();
      App.editing = null;
      // Snapshot the current binding so a rejected rebind can be reverted —
      // never apply an unvalidated combo on failure.
      const prev = App.shortcuts[which];
      Promise.resolve(api.rebind_shortcut(which, combo)).then((res) => {
        if (res && res.keys) combo.keys = res.keys;
        if (!res || res.ok !== false) App.shortcuts[which] = combo;
        renderSettings();
        renderPanel();
      }).catch(() => {
        // Rebind failed — keep the previous shortcut, don't apply the new combo.
        App.shortcuts[which] = prev;
        App.shortcutHint = "Couldn't change shortcut — try a different key.";
        renderSettings();
        setTimeout(() => { App.shortcutHint = null; renderSettings(); }, 2400);
      });
    };
    App._captureHandler = cap;
    window.addEventListener("keydown", cap, true);
  }
  function teardownCapture() {
    if (App._captureHandler) {
      window.removeEventListener("keydown", App._captureHandler, true);
      App._captureHandler = null;
    }
  }
  function cancelEdit() {
    teardownCapture();
    App.editing = null;
    renderSettings();
  }

  /* ════════════════════════════════════════════════════════════════════
     STATE INGEST + PUBLIC BACKEND-FACING FUNCTIONS (the contract)
     ════════════════════════════════════════════════════════════════════ */
  function ingestState(state) {
    if (!state) return;
    if (Array.isArray(state.entries)) App.entries = state.entries;
    if (state.settings) App.settings = state.settings;
    if (state.engine) App.engine = state.engine;
    if (Array.isArray(state.mics)) App.mics = state.mics;
    if (state.shortcuts) App.shortcuts = state.shortcuts;
    if (state.theme) App.theme = state.theme;
    if (state.effectiveTheme) applyTheme(state.effectiveTheme);
    else applyTheme(App.theme === "light" ? "light" : "dark");
    if (App.settings && typeof App.settings.transparency === "number") {
      applyTransparency(App.settings.transparency);
    }
  }

  // window.tiroApplyState(state) — re-render everything from a get_state()-shaped object
  window.tiroApplyState = function (state) {
    ingestState(state);
    renderPanel();
    renderSettings();
  };

  // window.tiroAddEntry(entry) — prepend one entry with the .fresh entrance animation
  window.tiroAddEntry = function (entry) {
    if (!entry) return;
    const e = Object.assign({}, entry, { fresh: true });
    App.entries.unshift(e);
    renderPanel();
    // settle: drop the .fresh flag after the entrance animation
    setTimeout(() => {
      const found = App.entries.find((x) => x.id === e.id);
      if (found) { found.fresh = false; renderPanel(); }
    }, 600);
  };

  // window.tiroSetEngine(engine) — update footer chip + Settings "Now running"
  window.tiroSetEngine = function (engine) {
    applyEngine(engine);
    renderPanel();
    renderSettings();
  };

  // window.tiroSetTheme(effective) — "light"|"dark" -> data-theme on <html>
  window.tiroSetTheme = function (effective) {
    applyTheme(effective);
  };

  // window.tiroSetRecording(on) — reflect record button (mic <-> stop + accent)
  window.tiroSetRecording = function (on) {
    App.recording = !!on;
    renderPanel();
  };

  // window.tiroSetStorage(obj) — { fallback: bool, path: str } pushed when a
  // transcription finishes writing. Mirror it into settings so the Storage card
  // shows the calm inline banner ONLY while obj.fallback is true.
  window.tiroSetStorage = function (obj) {
    if (!obj) return;
    App.settings.storageFallback = !!obj.fallback;
    if (typeof obj.path === "string") App.settings.storagePath = obj.path;
    renderSettings();
  };

  // window.tiroModelProgress({ model, pct, done, error }) — pushed by the
  // backend while a model downloads (~every 1%). Progress updates the row in
  // place; completion re-queries list_models so the row flips to Installed
  // and the battery/plugged selectors pick the model up.
  window.tiroModelProgress = function (p) {
    if (!p || !p.model) return;
    if (p.done) {
      delete App.modelProgress[p.model];
      const m = App.models.find((x) => x.name === p.model);
      if (m) { m.installed = true; m.downloading = false; }
      refreshModels();
      return;
    }
    App.modelProgress[p.model] = p;
    updateModelRow(p.model);
  };

  /* ════════════════════════════════════════════════════════════════════
     BOOT
     ════════════════════════════════════════════════════════════════════ */
  // Recompute bridge availability at call time — HAS_BRIDGE is only the
  // parse-time snapshot and the bridge can arrive after this script runs.
  function bridgeReady() {
    return !!(window.pywebview && window.pywebview.api);
  }

  let _booted = false;
  function boot() {
    if (_booted) return;            // guard against a double boot flash
    _booted = true;
    if (bridgeReady()) api = window.pywebview.api;
    Promise.resolve(api.get_state()).then((state) => {
      ingestState(state);
      renderPanel();
      renderSettings();
      refreshModels();
    }).catch((err) => {
      // last-resort: render with whatever defaults we have
      ingestState(MOCK_STATE);
      renderPanel();
      renderSettings();
    });
  }

  // Render a neutral empty skeleton so a truly-standalone preview (no bridge)
  // isn't blank while we wait — replaced the moment boot() runs.
  function renderSkeleton() {
    ingestState(MOCK_STATE);
    renderPanel();
    renderSettings();
  }

  // pywebview injects window.pywebview.api asynchronously; the `pywebviewready`
  // event fires once the bridge is live. Boot ONCE — eagerly if the bridge is
  // already here, otherwise wait for it and only fall back to standalone if it
  // never arrives (so there's no mock->real double render flash).
  if (bridgeReady()) {
    boot();
  } else {
    window.addEventListener("pywebviewready", boot);
    // Neutral placeholder until the bridge arrives (or we give up below).
    renderSkeleton();
    // If no bridge ever shows up (real standalone browser preview), boot the
    // mock so the page is interactive instead of frozen on the skeleton.
    setTimeout(function () { if (!_booted) boot(); }, 1500);
  }

})();
