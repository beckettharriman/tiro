/* app.js — TIRO panel: compact surface + advanced area (Transcribe /
   Settings / Vocabulary / Models). Vanilla JS over the static DOM in
   index.html; visual mechanics mirror the design source exactly, data
   flows through the pywebview-shaped bridge (or the built-in mock when
   previewing over file:// with no backend). */
(function () {
  "use strict";

  function $(id) { return document.getElementById(id); }

  /* ── tiny DOM helper ─────────────────────────────────────────────────── */
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
        } else n.setAttribute(k, v);
      }
    }
    if (children != null) {
      (Array.isArray(children) ? children : [children]).forEach((c) => {
        if (c == null || c === false) return;
        n.appendChild(typeof c === "string" || typeof c === "number"
          ? document.createTextNode(String(c)) : c);
      });
    }
    return n;
  }

  /* ── shared svg bits (from the design source) ────────────────────────── */
  const checkSvg = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6 9 17l-5-5"/></svg>';
  const xSvg = '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><line x1="18" y1="6" x2="6" y2="18"/><line x1="6" y1="6" x2="18" y2="18"/></svg>';
  const arrSvg = '<svg class="arr" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M5 12h14"/><path d="m12 5 7 7-7 7"/></svg>';

  /* ── shortcut helpers (code-based, matches the backend combo shape) ──── */
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

  /* ── transparency: setting t (0 solid … 100 most see-through) <-> the
     glass alpha the design's slider shows (0.95 … 0.30) ─────────────────── */
  function tToAlpha(t) { return 0.95 - 0.65 * (Math.max(0, Math.min(100, t)) / 100); }
  function alphaToT(a) { return Math.round((0.95 - Math.max(0.30, Math.min(0.95, a))) / 0.65 * 100); }

  /* ── reduced motion: skip settle waits and smooth scrolling ──────────── */
  const _rmq = window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)");
  function motionReduced() { return !!(_rmq && _rmq.matches); }

  /* ════════════════════════════════════════════════════════════════════
     API — real pywebview bridge, or a MOCK for standalone file:// preview
     ════════════════════════════════════════════════════════════════════ */
  const HAS_BRIDGE = !!(window.pywebview && window.pywebview.api);
  function bridgeReady() { return !!(window.pywebview && window.pywebview.api); }

  function isoDay(d) {
    return d.getFullYear() + "-" + String(d.getMonth() + 1).padStart(2, "0") +
      "-" + String(d.getDate()).padStart(2, "0");
  }
  const MOCK_DAYS = (() => {
    const now = new Date();
    const day = (offset) => { const d = new Date(now); d.setDate(d.getDate() + offset); return isoDay(d); };
    const y = now.getFullYear();
    return {
      today: day(0), yesterday: day(-1),
      jul24: y + "-07-24", jun30: y + "-06-30", mar3: y + "-03-03"
    };
  })();
  const MOCK_ENTRIES = [
    { id: "m1", day: MOCK_DAYS.today, clock: "10:42 PM", dur: "0:11", text: "Remind me to swap the joycon shells on the OLED before I list it, and check whether the back plate screws are stripped." },
    { id: "m2", day: MOCK_DAYS.today, clock: "10:31 PM", dur: "0:29", text: "Okay so for the vault entry tonight: I spent most of the day on the Tiro redesign, mostly arguing with myself about whether the advanced panel should slide or grow in place. Growing in place feels right because the panel never loses its anchor point, and the whole identity of the app is that it stays where you put it and never surprises you. Also need to remember to file the receipt from Micro Center." },
    { id: "m3", day: MOCK_DAYS.today, clock: "9:58 PM", dur: "0:05", text: "Add vulkan headers to the build docs." },
    { id: "m4", day: MOCK_DAYS.today, clock: "9:12 PM", dur: "0:14", text: "Draft reply to the guy asking about the Steam Deck: it's the 512 gig model, screen has zero scratches, comes with the case and the original box." },
    { id: "m5", day: MOCK_DAYS.yesterday, clock: "4:20 PM", dur: "0:08", text: "Order thermal pads before the weekend, the 1.5 millimeter ones, not the 2s." },
    { id: "m6", day: MOCK_DAYS.yesterday, clock: "11:05 AM", dur: "0:22", text: "Meeting note: Priya wants the export flow demoed Thursday. Keep it under five minutes, lead with the clipboard story, skip the settings tour unless she asks." },
    { id: "m7", day: MOCK_DAYS.jul24, clock: "8:47 PM", dur: "0:12", text: "Idea: the pill could dim instead of hide when a video call is fullscreen, so you still know it's armed." },
    { id: "m8", day: MOCK_DAYS.jun30, clock: "2:33 PM", dur: "0:19", text: "Vault entry: switched the whole build to static linking today. Binary is 40 megs heavier but installs are one file now, which is the point." },
    { id: "m9", day: MOCK_DAYS.mar3, clock: "9:15 AM", dur: "0:07", text: "Call the dentist back about moving the Tuesday appointment." }
  ];

  const MOCK_STATE = {
    entries: MOCK_ENTRIES.filter((e) => e.day === MOCK_DAYS.today).map((e) => ({ id: e.id, clock: e.clock, dur: e.dur, text: e.text })),
    settings: {
      powerMode: "auto", modelBattery: "base.en", modelPlugged: "small.en",
      soundCues: true, volume: 60, recordingPill: true,
      pillPosition: "bottom", pillPadding: 110,
      clipboardCleanup: "light", smartVocab: true,
      micName: "MacBook Pro Microphone", launchAtLogin: true,
      saveTranscripts: true, savePath: "~/Documents/Tiro", transparency: 35,
      storageFallback: false, storagePath: ""
    },
    engine: { model: "small.en", device: "GPU", power: "plugged" },
    mics: ["MacBook Pro Microphone", "AirPods Pro", "Shure MV7"],
    shortcuts: {
      dictate: { ctrl: true, alt: true, shift: false, meta: false, code: "Space", keys: ["Ctrl", "Alt", "Space"] },
      paste:   { ctrl: true, alt: true, shift: false, meta: false, code: "KeyV", keys: ["Ctrl", "Alt", "V"] },
      panel:   { ctrl: true, alt: true, shift: false, meta: false, code: "KeyC", keys: ["Ctrl", "Alt", "C"] },
      cancel:  { ctrl: true, alt: true, shift: false, meta: false, code: "KeyX", keys: ["Ctrl", "Alt", "X"] }
    },
    theme: "dark", effectiveTheme: "dark",
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
    ],
    vocab: {
      hotwords: ["Beckett", "Tiro", "Tauri", "joycon", "PipeWire", "Fedora", "MedStar", "OLED", "Vulkan", "whisper"],
      corrections: [["jira", "Jira"], ["tyro", "Tiro"], ["pipe wire", "PipeWire"], ["joy con", "Joy-Con"], ["med star", "MedStar"]]
    }
  };

  function clone(o) { return JSON.parse(JSON.stringify(o)); }

  const MockApi = {
    _state: clone(MOCK_STATE),
    _recording: false,
    _cancelled: {},
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
    get_state() {
      this._deriveEngine();
      const s = clone(this._state);
      delete s.models; delete s.vocab;
      return Promise.resolve(s);
    },
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
    toggle_record() {
      // preview: mirror the backend's pushes so the whole flow is visible
      this._recording = !this._recording;
      if (window.tiroSetRecording) window.tiroSetRecording(this._recording);
      if (!this._recording && window.tiroAddEntry) {
        const now = new Date();
        let hh = now.getHours();
        const ap = hh >= 12 ? "PM" : "AM"; hh = hh % 12 || 12;
        window.tiroAddEntry({
          id: "m" + Date.now(),
          clock: hh + ":" + String(now.getMinutes()).padStart(2, "0") + " " + ap,
          dur: "0:07",
          text: "Note to self: the pill should fade out half a second after the copy lands, not instantly — it reads as more deliberate."
        });
      }
      return Promise.resolve(null);
    },
    cancel_record() { return Promise.resolve(null); },
    set_pin() { return Promise.resolve(null); },
    set_expanded() { return Promise.resolve(null); },
    close_panel() { return Promise.resolve(null); },
    begin_drag() { return Promise.resolve(null); },
    pick_folder() { return Promise.resolve(null); },
    rebind_shortcut(which, combo) {
      // reject a chord already claimed by another shortcut, like the backend
      const clash = Object.keys(this._state.shortcuts).some((k) =>
        k !== which &&
        (this._state.shortcuts[k].keys || []).join("+") === (combo.keys || []).join("+"));
      if (clash) return Promise.resolve({ ok: false, keys: combo.keys });
      this._state.shortcuts[which] = combo;
      return Promise.resolve({ ok: true, keys: combo.keys });
    },
    history_days() {
      const days = MOCK_ENTRIES.map((e) => e.day).filter((d, i, a) => a.indexOf(d) === i);
      return Promise.resolve(days);
    },
    history_entries(day) {
      return Promise.resolve(MOCK_ENTRIES.filter((e) => e.day === day)
        .map((e) => ({ id: e.id, clock: e.clock, dur: e.dur, text: e.text })));
    },
    list_vocab() { return Promise.resolve(clone(this._state.vocab)); },
    set_vocab(hotwords, corrections) {
      this._state.vocab = { hotwords: clone(hotwords), corrections: clone(corrections) };
      return Promise.resolve({ ok: true });
    },
    list_models() { return Promise.resolve(clone(this._state.models)); },
    download_model(name) {
      const m = this._state.models.find((x) => x.name === name);
      if (!m || m.installed) return Promise.resolve({ ok: true, installed: true });
      delete this._cancelled[name];
      let pct = 0;
      const tick = () => {
        if (this._cancelled[name]) {
          delete this._cancelled[name];
          if (window.tiroModelProgress) window.tiroModelProgress({ model: name, pct: 0, done: false, error: null, cancelled: true });
          return;
        }
        pct += 4;
        if (pct >= 100) {
          m.installed = true;
          if (window.tiroModelProgress) window.tiroModelProgress({ model: name, pct: 100, done: true, error: null });
        } else {
          if (window.tiroModelProgress) window.tiroModelProgress({ model: name, pct: pct, done: false, error: null });
          setTimeout(tick, 160);
        }
      };
      setTimeout(tick, 160);
      return Promise.resolve({ ok: true, started: true });
    },
    cancel_download(name) { this._cancelled[name] = true; return Promise.resolve({ ok: true }); }
  };

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
    adv: false,
    view: "settings",
    models: [],
    modelProgress: {},           // model name -> latest progress payload
    vocab: { hotwords: [], corrections: [] },
    days: [],                    // iso days, newest first; [0] is always today
    dayIdx: 0,                   // day the jumper points at (scroll-spied)
    histLoaded: 0,               // index of the oldest day section rendered
    dayCache: {},                // iso day -> entries (today reads App.entries live)
    allDaysLoaded: false
  };

  const panel = $("panel");
  const listEl = $("list");
  const advList = $("advList");
  const searchInput = $("searchInput");
  const searchBox = document.querySelector(".search");

  /* ════════════════════════════════════════════════════════════════════
     ENTRIES (compact list + advanced Transcribe view)
     ════════════════════════════════════════════════════════════════════ */
  function todayIso() { return isoDay(new Date()); }

  function dayLabel(iso) {
    if (!iso) return "";
    if (iso === todayIso()) return "Today";
    const now = new Date();
    const yest = new Date(now); yest.setDate(yest.getDate() - 1);
    if (iso === isoDay(yest)) return "Yesterday";
    const parts = iso.split("-").map(Number);
    const months = ["January", "February", "March", "April", "May", "June",
      "July", "August", "September", "October", "November", "December"];
    const label = months[(parts[1] || 1) - 1] + " " + parts[2];
    return parts[0] === now.getFullYear() ? label : label + ", " + parts[0];
  }

  function makeEntry(e, landing, showDay) {
    const el = document.createElement("div");
    el.className = "entry" + (landing ? " landing" : "");
    el.dataset.id = e.id;
    el.innerHTML =
      '<div class="meta">' + (showDay && e.dayIso ? '<span class="day"></span><span class="dot">·</span>' : "") +
      '<span class="time"></span><span class="dot">·</span><span class="dur"></span>' +
      '<span class="copied-chip">' + checkSvg + "Copied</span></div>" +
      '<div class="txt clamped"></div>' +
      '<button class="showmore">Show more</button>';
    if (showDay && e.dayIso) el.querySelector(".day").textContent = dayLabel(e.dayIso);
    el.querySelector(".time").textContent = e.clock;
    el.querySelector(".dur").textContent = e.dur;
    el.querySelector(".txt").textContent = e.text;
    const txt = el.querySelector(".txt");
    const more = el.querySelector(".showmore");
    more.addEventListener("click", (ev) => {
      ev.stopPropagation();
      const open = el.classList.toggle("expanded");
      more.textContent = open ? "Show less" : "Show more";
    });
    el.addEventListener("click", () => {
      Promise.resolve(api.copy_text(e.text)).catch(() => {});
      flashCopied(el);
    });
    requestAnimationFrame(() => {
      if (txt.scrollHeight - txt.clientHeight > 4) el.classList.add("clampable");
    });
    return el;
  }

  let copyTimer;
  function flashCopied(el) {
    document.querySelectorAll(".entry.copied").forEach((x) => x.classList.remove("copied"));
    el.classList.add("copied");
    clearTimeout(copyTimer);
    copyTimer = setTimeout(() => el.classList.remove("copied"), 1600);
  }

  function renderEntries() {
    listEl.innerHTML = "";
    App.entries.forEach((e) => listEl.appendChild(makeEntry(e)));
    panel.classList.toggle("is-empty", !App.entries.length);
  }

  function entriesForDay(iso) {
    if (iso === todayIso()) return Promise.resolve(App.entries);
    if (App.dayCache[iso]) return Promise.resolve(App.dayCache[iso]);
    return Promise.resolve(api.history_entries(iso)).then((list) => {
      const arr = Array.isArray(list) ? list : [];
      App.dayCache[iso] = arr;
      return arr;
    }).catch(() => []);
  }

  function loadDays() {
    return Promise.resolve(api.history_days()).then((days) => {
      const next = Array.isArray(days) ? days.slice() : [];
      // the backend only lists days that have transcripts on disk — today
      // leads the list even before its first take (one-day default view)
      if (next[0] !== todayIso()) next.unshift(todayIso());
      // day set changed (first take of a day, or midnight rolled a new
      // today in): search must re-walk the days — the ex-today day now has
      // a JSONL of its own and is fetched like any other day, it was never
      // in dayCache while it was live
      if (next.join("\n") !== App.days.join("\n")) App.allDaysLoaded = false;
      App.days = next;
      if (App.histLoaded >= App.days.length) App.histLoaded = App.days.length - 1;
      if (App.dayIdx >= App.days.length) App.dayIdx = 0;
    }).catch(() => { App.days = [todayIso()]; App.histLoaded = 0; App.dayIdx = 0; });
  }

  function ensureAllDays() {
    if (App.allDaysLoaded) return Promise.resolve();
    return Promise.all(App.days.map((d) => entriesForDay(d)))
      .then(() => { App.allDaysLoaded = true; });
  }

  /* One day section: sticky header + that day's entries. Only today can
     be empty (older days come from history_days, which lists only days
     with transcripts on disk) — its empty state points back at the older
     days one scroll away. */
  function daySection(iso, list) {
    const sec = h("section", { class: "dayseg", "data-day": iso },
      h("div", { class: "dayhdr", text: dayLabel(iso) }));
    const body = h("div", { class: "daybody" });
    list.forEach((e) => body.appendChild(makeEntry(e)));
    if (!list.length) {
      body.appendChild(h("div", {
        class: "dayempty",
        text: App.days.length > 1
          ? "No transcripts yet today — scroll down for earlier days."
          : "No transcripts yet today."
      }));
    }
    sec.appendChild(body);
    return sec;
  }

  /* The advanced History view opens on TODAY only; older days append one
     at a time as you scroll back (or via the day jumper), each fetched
     once through history_entries and cached. */
  let advToken = 0;
  function renderAdv() {
    const token = ++advToken;
    const q = (searchInput.value || "").trim().toLowerCase();
    const pager = $("pager");
    if (q) {
      ensureAllDays().then(() => {
        if (token !== advToken) return;
        advList.innerHTML = "";
        const hits = [];
        App.days.forEach((d) => {
          const src = d === todayIso() ? App.entries : (App.dayCache[d] || []);
          src.forEach((e) => {
            const label = dayLabel(d).toLowerCase();
            if (e.text.toLowerCase().includes(q) || e.clock.toLowerCase().includes(q) || label.includes(q)) {
              hits.push(Object.assign({}, e, { dayIso: d }));
            }
          });
        });
        hits.forEach((e) => advList.appendChild(makeEntry(e, false, true)));
        $("noRes").style.display = hits.length ? "none" : "block";
        $("noResQ").textContent = searchInput.value.trim();
        pager.style.display = "none";
      });
      return;
    }
    $("noRes").style.display = "none";
    if (App.histLoaded >= App.days.length) App.histLoaded = Math.max(0, App.days.length - 1);
    const daysToShow = App.days.slice(0, App.histLoaded + 1);
    Promise.all(daysToShow.map(entriesForDay)).then((lists) => {
      if (token !== advToken) return;
      advList.innerHTML = "";
      daysToShow.forEach((d, i) => advList.appendChild(daySection(d, lists[i])));
      pager.style.display = "flex";
      syncPager();
    });
  }

  const mainEl = $("main");
  function daySections() {
    return Array.prototype.slice.call(advList.querySelectorAll(".dayseg"));
  }
  function histScrolling() {
    return App.adv && App.view === "history" && !searchInput.value.trim();
  }

  /* Append the next older day below the ones already shown. */
  let histLoading = false;
  function loadOlderDay() {
    if (histLoading || !histScrolling() || App.histLoaded >= App.days.length - 1) {
      return Promise.resolve(false);
    }
    histLoading = true;
    const idx = App.histLoaded + 1;
    const iso = App.days[idx];
    return entriesForDay(iso).then((list) => {
      histLoading = false;
      if (!histScrolling() || App.histLoaded >= idx) return true;
      App.histLoaded = idx;
      advList.appendChild(daySection(iso, list));
      syncPager();
      return true;
    }, () => { histLoading = false; return false; });
  }

  /* Day jumper: label follows the day under the top of the viewport;
     Older loads/scrolls one day back, Newer scrolls one day forward. */
  function syncPager() {
    const day = App.days[App.dayIdx] || todayIso();
    $("pagerDay").textContent = dayLabel(day);
    $("pagerNewer").disabled = App.dayIdx === 0;
    $("pagerOlder").disabled = App.dayIdx >= App.days.length - 1;
  }
  function syncPagerFromScroll() {
    const secs = daySections();
    if (!secs.length) return;
    const mtop = mainEl.getBoundingClientRect().top;
    let idx = 0;
    secs.forEach((s, i) => { if (s.getBoundingClientRect().top - mtop <= 40) idx = i; });
    if (idx !== App.dayIdx) { App.dayIdx = idx; syncPager(); }
  }
  function scrollToDay(idx) {
    const sec = daySections()[idx];
    if (!sec) return;
    const mtop = mainEl.getBoundingClientRect().top;
    const y = mainEl.scrollTop + (sec.getBoundingClientRect().top - mtop) - 4;
    mainEl.scrollTo({ top: Math.max(0, y), behavior: motionReduced() ? "auto" : "smooth" });
    App.dayIdx = idx;
    syncPager();
  }

  mainEl.addEventListener("scroll", () => {
    if (!histScrolling()) return;
    syncPagerFromScroll();
    if (mainEl.scrollTop + mainEl.clientHeight >= mainEl.scrollHeight - 120) loadOlderDay();
  });
  /* a short day never overflows, so bottom-of-scroll alone can't reach the
     past — a downward wheel at the bottom (or with nothing to scroll) also
     pulls in the previous day */
  mainEl.addEventListener("wheel", (e) => {
    if (!histScrolling() || e.deltaY <= 0) return;
    if (mainEl.scrollTop + mainEl.clientHeight >= mainEl.scrollHeight - 4) loadOlderDay();
  }, { passive: true });

  searchInput.addEventListener("input", () => {
    searchBox.classList.toggle("hasq", !!searchInput.value.trim());
    renderAdv();
  });
  $("searchClr").addEventListener("click", () => {
    searchInput.value = "";
    searchBox.classList.remove("hasq");
    renderAdv();
    searchInput.focus();
  });
  $("pagerOlder").addEventListener("click", () => {
    const target = Math.min(App.dayIdx + 1, App.days.length - 1);
    if (target <= App.histLoaded) { scrollToDay(target); return; }
    loadOlderDay().then((ok) => { if (ok) scrollToDay(App.histLoaded); });
  });
  $("pagerNewer").addEventListener("click", () => {
    if (App.dayIdx > 0) scrollToDay(App.dayIdx - 1);
  });

  /* ════════════════════════════════════════════════════════════════════
     MICS — compact dropdown + the two settings selects, one source
     ════════════════════════════════════════════════════════════════════ */
  const micCap = $("micCap"), micMenu = $("micMenu"), micName = $("micName");
  const selMic = $("selMic"), selMicT = $("selMicT");

  function micList() {
    const names = App.mics.length ? App.mics.slice() : [];
    const cur = App.settings.micName;
    if (cur && names.indexOf(cur) < 0) names.unshift(cur);
    return names;
  }

  function renderMics() {
    const names = micList();
    const cur = App.settings.micName || names[0] || "";
    micName.textContent = cur || "Default microphone";
    micMenu.innerHTML = "";
    names.forEach((m) => {
      const b = document.createElement("button");
      b.className = "mic-item"; b.setAttribute("role", "option");
      b.setAttribute("aria-selected", m === cur);
      b.innerHTML = "<span></span>" + checkSvg;
      b.querySelector("span").textContent = m;
      b.addEventListener("click", () => setMic(m, true));
      micMenu.appendChild(b);
    });
    fillSelect(selMic, names, cur);
    fillSelect(selMicT, names, cur);
    const sub = $("levelSub");
    if (sub) sub.textContent = cur ? "Live from " + cur + "." : "Live from the selected microphone.";
  }

  function setMic(name, closeMenu) {
    if (closeMenu) setMicOpen(false);
    setSetting("micName", name);
    renderMics();
    restartMic();
  }
  function setMicOpen(open) {
    panel.classList.toggle("mic-open", open);
    micCap.setAttribute("aria-expanded", open);
  }
  micCap.addEventListener("click", (e) => { e.stopPropagation(); setMicOpen(!panel.classList.contains("mic-open")); });
  document.addEventListener("click", (e) => { if (!micMenu.contains(e.target)) setMicOpen(false); });

  /* ════════════════════════════════════════════════════════════════════
     HEADER — pin / expand / close / drag
     ════════════════════════════════════════════════════════════════════ */
  const pinBtn = $("pinBtn");
  pinBtn.addEventListener("click", () => {
    App.pinned = !App.pinned;
    pinBtn.setAttribute("aria-pressed", String(App.pinned));
    Promise.resolve(api.set_pin(App.pinned)).catch(() => {});
  });
  $("closeBtn").addEventListener("click", () => { Promise.resolve(api.close_panel()).catch(() => {}); });
  $("hdr").addEventListener("mousedown", (ev) => {
    if (ev.button !== 0) return;
    if (ev.target && ev.target.closest("button")) return;
    ev.preventDefault();
    Promise.resolve(api.begin_drag && api.begin_drag()).catch(() => {});
  });

  /* ── expand / collapse: two surfaces move here — the OS window (instant,
     transparent, top-right corner fixed by the backend) and the glass
     panel (CSS width, 520 ms). The window must never be the thing the eye
     sees move:
       grow:   window jumps to 800 FIRST, then the glass animates 400->800
               inside the already-big window once the viewport is actually
               wide (resize event with a deadline backstop — the invoke ack
               only means the resize was scheduled, so starting on the ack
               alone clips the animation);
       shrink: the glass animates down first, the window snaps to 400 only
               after the 520 ms settle (instantly under reduced motion).
     While the window is wider than the glass (~520 ms per direction) the
     transparent margin still belongs to the panel window and eats clicks —
     accepted: no cross-platform per-pixel input shaping, and at rest the
     window always matches the glass exactly. ──────────────────────────── */
  const expandBtn = $("expandBtn");
  let advTimer = null;
  let advGen = 0;
  /* run fn once the viewport is at least px wide — immediately if it
     already is, else on the resize event the native window change fires,
     with a deadline fallback so a missed resize can never wedge expand */
  function whenWide(px, deadlineMs, fn) {
    if (window.innerWidth >= px) { fn(); return; }
    let fired = false;
    const finish = () => {
      if (fired) return;
      fired = true;
      window.removeEventListener("resize", check);
      clearTimeout(tm);
      fn();
    };
    const check = () => { if (window.innerWidth >= px) finish(); };
    const tm = setTimeout(finish, deadlineMs);
    window.addEventListener("resize", check);
  }
  function setAdv(on) {
    if (App.adv === on) return;
    App.adv = on;
    clearTimeout(advTimer);
    const gen = ++advGen;
    if (on) {
      expandBtn.title = "Collapse";
      renderAdv();
      ensureMic();
      Promise.resolve(api.set_expanded && api.set_expanded(true)).catch(() => {}).then(() => {
        whenWide(780, 500, () => {
          if (gen !== advGen) return; /* collapsed again before the window grew */
          panel.classList.add("adv");
        });
      });
    } else {
      panel.classList.remove("adv");
      expandBtn.title = "Expand";
      advTimer = setTimeout(() => {
        Promise.resolve(api.set_expanded && api.set_expanded(false)).catch(() => {});
      }, motionReduced() ? 0 : 560);
    }
  }
  expandBtn.addEventListener("click", () => {
    const opening = !panel.classList.contains("adv");
    if (opening) setView("history");
    setAdv(opening);
  });
  $("gearBtn").addEventListener("click", () => { setView("settings"); setAdv(true); });

  /* sidebar nav */
  function setView(v) {
    App.view = v;
    document.querySelectorAll(".navitem").forEach((n) => n.classList.toggle("on", n.dataset.view === v));
    document.querySelectorAll(".view").forEach((x) => x.classList.remove("on"));
    const view = $("view-" + v);
    void view.offsetWidth; /* restart entrance stagger */
    view.classList.add("on");
    $("main").scrollTop = 0;
    if (v === "history") {
      /* one-day default: every visit starts at today only */
      App.histLoaded = 0;
      App.dayIdx = 0;
      loadDays().then(renderAdv);
    }
    if (v === "vocab") loadVocab();
    if (v === "models") refreshModels();
  }
  document.querySelectorAll(".navitem").forEach((n) => n.addEventListener("click", () => setView(n.dataset.view)));

  /* ════════════════════════════════════════════════════════════════════
     SETTINGS PLUMBING
     ════════════════════════════════════════════════════════════════════ */
  function setSetting(key, value) {
    if (key === "theme") App.theme = value;
    else App.settings[key] = value;
    Promise.resolve(api.set_setting(key, value)).then((res) => {
      if (!res) return;
      if (res.engine && (res.engine.model !== App.engine.model ||
          res.engine.device !== App.engine.device ||
          res.engine.power !== App.engine.power)) {
        App.engine = res.engine;
        updateEngine();
      }
      if (res.theme != null && res.theme !== App.theme) { App.theme = res.theme; syncThemeSeg(); }
      if (res.effectiveTheme) applyTheme(res.effectiveTheme);
      if (res.launchAtLogin != null && res.launchAtLogin !== App.settings.launchAtLogin) {
        App.settings.launchAtLogin = res.launchAtLogin;
        setSw($("swLogin"), res.launchAtLogin);
      }
    }).catch(() => {});
  }

  function applyTheme(effective) {
    document.documentElement.setAttribute("data-theme", effective === "light" ? "light" : "dark");
  }
  function applyTransparency(t) {
    document.documentElement.style.setProperty("--glass-a", tToAlpha(t).toFixed(2));
  }

  function powerWord(power) { return power === "plugged" ? "plugged in" : "battery"; }

  /* engine chips (compact footer + sidebar) and the Now-running row */
  function updateEngine() {
    document.querySelectorAll(".engine").forEach((ch) => {
      ch.textContent = App.engine.model + " · ";
      ch.appendChild(h("b", { text: App.engine.device }));
    });
    const nr = $("nowRun");
    nr.textContent = App.engine.model + " · ";
    nr.appendChild(h("b", { text: App.engine.device }));
    if (App.settings.powerMode === "auto") {
      nr.appendChild(document.createTextNode(" · " + powerWord(App.engine.power)));
    }
  }

  /* switches */
  function setSw(sw, on) { sw.setAttribute("aria-checked", String(!!on)); }
  function swOn(sw) { return sw.getAttribute("aria-checked") === "true"; }
  const swWiring = {
    swCues: (on) => { setSetting("soundCues", on); $("rowVolume").classList.toggle("disabled", !on); },
    swPill: (on) => { setSetting("recordingPill", on); syncPillRows(); },
    swVocab: (on) => setSetting("smartVocab", on),
    swSave: (on) => setSetting("saveTranscripts", on),
    swLogin: (on) => setSetting("launchAtLogin", on)
  };
  Object.keys(swWiring).forEach((id) => {
    const sw = $(id);
    sw.addEventListener("click", () => {
      const on = !swOn(sw);
      setSw(sw, on);
      swWiring[id](on);
    });
  });
  function syncPillRows() {
    const off = !swOn($("swPill"));
    $("rowPillPos").classList.toggle("disabled", off);
    $("rowPillDist").classList.toggle("disabled", off);
  }

  /* segmented controls */
  function segSet(seg, value) {
    seg.querySelectorAll("button").forEach((b) => b.classList.toggle("on", b.dataset.value === value));
  }
  document.querySelectorAll(".seg").forEach((seg) => {
    seg.querySelectorAll("button").forEach((b) => b.addEventListener("click", () => {
      seg.querySelectorAll("button").forEach((x) => x.classList.remove("on"));
      b.classList.add("on");
      const v = b.dataset.value;
      if (seg.dataset.seg === "power") { setSetting("powerMode", v); updateEngine(); }
      if (seg.dataset.seg === "pillpos") setSetting("pillPosition", v);
      if (seg.dataset.seg === "cleanup") { setSetting("clipboardCleanup", v); setCleanupCopy(v); }
      if (seg.dataset.seg === "theme") {
        setSetting("theme", v);
        if (v === "light" || v === "dark") applyTheme(v);
      }
    }));
  });
  function syncThemeSeg() { segSet($("themeSeg"), App.theme); }

  /* clipboard cleanup — dynamic helper copy */
  const cleanupCopy = {
    off: "Text lands on the clipboard exactly as transcribed.",
    light: "Light cleanup fixes spacing and capitalization on copy.",
    fillers: "Also strips filler words — um, uh, you know — on copy."
  };
  const cleanupSub = $("cleanupSub");
  function setCleanupCopy(mode) {
    cleanupSub.classList.add("swapping");
    setTimeout(() => {
      cleanupSub.textContent = cleanupCopy[mode] || cleanupCopy.light;
      cleanupSub.classList.remove("swapping");
    }, 170);
  }

  /* sliders: track fill + readouts */
  function syncFill(r) {
    const min = parseFloat(r.min), max = parseFloat(r.max);
    const pct = (parseFloat(r.value) - min) / (max - min) * 100;
    r.style.setProperty("--fill", pct + "%");
  }
  document.querySelectorAll(".rng").forEach((r) => { syncFill(r); r.addEventListener("input", () => syncFill(r)); });

  const rngVol = $("rngVol");
  rngVol.addEventListener("input", () => { $("volVal").textContent = rngVol.value + "%"; });
  rngVol.addEventListener("change", () => setSetting("volume", Number(rngVol.value)));

  const rngDist = $("rngDist");
  let distTimer = null;
  rngDist.addEventListener("input", () => {
    $("distVal").textContent = rngDist.value + " px";
    App.settings.pillPadding = Number(rngDist.value);
    if (distTimer !== null) clearTimeout(distTimer);
    distTimer = setTimeout(() => {
      distTimer = null;
      Promise.resolve(api.set_setting("pillPadding", App.settings.pillPadding)).catch(() => {});
    }, 200);
  });

  const rngGlass = $("rngGlass");
  let glassTimer = null;
  rngGlass.addEventListener("input", () => {
    const a = parseFloat(rngGlass.value);
    $("glassVal").textContent = a.toFixed(2);
    document.documentElement.style.setProperty("--glass-a", a.toFixed(2));
    App.settings.transparency = alphaToT(a);
    if (glassTimer !== null) clearTimeout(glassTimer);
    glassTimer = setTimeout(() => {
      glassTimer = null;
      Promise.resolve(api.set_setting("transparency", App.settings.transparency)).catch(() => {});
    }, 200);
  });

  /* ════════════════════════════════════════════════════════════════════
     CUSTOM DROPDOWNS (shared menu material, from the design source)
     ════════════════════════════════════════════════════════════════════ */
  function enhanceSelect(sel) {
    const wrap = sel.parentElement;
    if (wrap.closest(".toolbar")) wrap.classList.add("pill");
    const btn = document.createElement("button");
    btn.type = "button"; btn.className = sel.className;
    btn.innerHTML = '<span class="sellabel"></span>';
    const menu = document.createElement("div");
    menu.className = "ddmenu"; menu.setAttribute("role", "listbox");
    wrap.appendChild(btn); wrap.appendChild(menu);
    function refresh() {
      btn.querySelector(".sellabel").textContent = sel.options[sel.selectedIndex] ? sel.options[sel.selectedIndex].text : "";
      menu.innerHTML = "";
      Array.from(sel.options).forEach((o, i) => {
        const it = document.createElement("button");
        it.type = "button"; it.className = "dditem"; it.setAttribute("role", "option");
        it.setAttribute("aria-selected", i === sel.selectedIndex);
        it.innerHTML = "<span></span>" + checkSvg;
        it.querySelector("span").textContent = o.text;
        it.addEventListener("click", () => {
          sel.selectedIndex = i;
          wrap.classList.remove("open");
          refresh();
          sel.dispatchEvent(new Event("change"));
        });
        menu.appendChild(it);
      });
    }
    btn.addEventListener("click", (e) => {
      e.stopPropagation();
      const open = !wrap.classList.contains("open");
      document.querySelectorAll(".selwrap.open").forEach((w) => w.classList.remove("open"));
      wrap.classList.toggle("open", open);
    });
    sel.__ddRefresh = refresh;
    refresh();
  }
  document.querySelectorAll(".selwrap select").forEach(enhanceSelect);
  document.addEventListener("click", (e) => {
    document.querySelectorAll(".selwrap.open").forEach((w) => { if (!w.contains(e.target)) w.classList.remove("open"); });
  });

  function fillSelect(sel, options, value) {
    const want = options.join(" ") + "" + value;
    if (sel.__filled === want) return;
    sel.__filled = want;
    sel.innerHTML = "";
    options.forEach((o) => {
      const opt = document.createElement("option");
      opt.textContent = o;
      opt.selected = o === value;
      sel.appendChild(opt);
    });
    if (sel.__ddRefresh) sel.__ddRefresh();
  }

  selMic.addEventListener("change", () => setMic(selMic.options[selMic.selectedIndex].text, false));
  selMicT.addEventListener("change", () => setMic(selMicT.options[selMicT.selectedIndex].text, false));
  $("selBattery").addEventListener("change", function () {
    setSetting("modelBattery", this.options[this.selectedIndex].text);
    updateEngine();
  });
  $("selPlugged").addEventListener("change", function () {
    setSetting("modelPlugged", this.options[this.selectedIndex].text);
    updateEngine();
  });

  /* battery/plugged options: installed models plus the configured values */
  function modelOptions() {
    const opts = App.models.filter((m) => m.installed).map((m) => m.name);
    [App.settings.modelBattery, App.settings.modelPlugged].forEach((v) => {
      if (v && opts.indexOf(v) < 0) opts.push(v);
    });
    if (!opts.length) opts.push("base.en");
    return opts;
  }
  function syncModelSelects() {
    const opts = modelOptions();
    fillSelect($("selBattery"), opts, App.settings.modelBattery);
    fillSelect($("selPlugged"), opts, App.settings.modelPlugged);
  }

  /* ════════════════════════════════════════════════════════════════════
     MODELS VIEW
     ════════════════════════════════════════════════════════════════════ */
  function fmtSize(bytes) {
    if (!bytes) return "";
    if (bytes >= 1e9) return (bytes / 1e9).toFixed(1) + " GB";
    return Math.round(bytes / 1e6) + " MB";
  }

  function refreshModels() {
    Promise.resolve(api.list_models && api.list_models()).then((list) => {
      if (!Array.isArray(list)) return;
      App.models = list;
      renderModels();
      syncModelSelects();
    }).catch(() => {});
  }

  function modelState(m) {
    const p = App.modelProgress[m.name];
    if (p && p.error) return "failed";
    if (p && !p.done) return "downloading";
    if (m.installed) return "installed";
    if (m.downloading) return "downloading";
    return "idle";
  }

  function renderModelState(el) {
    const s = el.dataset.state;
    const name = el.dataset.model;
    if (s === "installed") {
      el.innerHTML = '<span class="installed">' + checkSvg + "Installed</span>";
    } else if (s === "idle") {
      el.innerHTML = '<button class="mini">Download</button>';
      el.querySelector(".mini").addEventListener("click", () => startDownload(name));
    } else if (s === "failed") {
      el.innerHTML = '<span class="failedtxt">Failed</span><button class="mini">Retry</button>';
      const p = App.modelProgress[name];
      if (p && p.error) el.querySelector(".failedtxt").title = p.error;
      el.querySelector(".mini").addEventListener("click", () => startDownload(name));
    } else if (s === "downloading") {
      el.innerHTML = '<span class="dl"><span class="bar"><i></i></span><span class="pct">0%</span>' +
        '<button class="ghost" title="Cancel">' + xSvg + "</button></span>";
      const p = App.modelProgress[name];
      const pct = p && p.pct ? p.pct : 0;
      el.querySelector(".bar i").style.width = pct + "%";
      el.querySelector(".pct").textContent = Math.round(pct) + "%";
      el.querySelector(".ghost").addEventListener("click", () => cancelDownload(name));
    }
  }

  function modelRow(m) {
    const row = h("div", { class: "row mrow" });
    const hint = m.hint + (m.sizeBytes ? " · " + fmtSize(m.sizeBytes) : "");
    row.appendChild(h("div", { class: "lbl" }, [
      h("div", { class: "name", text: m.name }),
      h("div", { class: "hint", text: hint })
    ]));
    const st = h("span", { class: "mstate" });
    st.dataset.model = m.name;
    st.dataset.state = modelState(m);
    renderModelState(st);
    row.appendChild(st);
    return row;
  }

  function renderModels() {
    const installedCard = $("installedCard"), modelsCard = $("modelsCard");
    installedCard.innerHTML = "";
    modelsCard.innerHTML = "";
    const installed = App.models.filter((m) => m.installed);
    const available = App.models.filter((m) => !m.installed);
    installed.forEach((m) => installedCard.appendChild(modelRow(m)));
    available.forEach((m) => modelsCard.appendChild(modelRow(m)));
    installedCard.closest(".grp").style.display = installed.length ? "" : "none";
    modelsCard.closest(".grp").style.display = available.length ? "" : "none";
  }

  function updateModelRow(name) {
    const el = document.querySelector('.mstate[data-model="' + name + '"]');
    const m = App.models.find((x) => x.name === name);
    if (!el || !m) return;
    const next = modelState(m);
    if (el.dataset.state !== next) {
      el.dataset.state = next;
      renderModelState(el);
      return;
    }
    if (next === "downloading") {
      const p = App.modelProgress[name];
      const pct = p && p.pct ? p.pct : 0;
      const bar = el.querySelector(".bar i"), lab = el.querySelector(".pct");
      if (bar) bar.style.width = pct + "%";
      if (lab) lab.textContent = Math.round(pct) + "%";
    }
  }

  function startDownload(name) {
    App.modelProgress[name] = { model: name, pct: 0, done: false, error: null };
    updateModelRow(name);
    Promise.resolve(api.download_model(name)).then((res) => {
      if (res && res.ok === false) {
        App.modelProgress[name] = { model: name, pct: 0, done: false, error: res.error || "Download failed" };
        updateModelRow(name);
      } else if (res && res.installed) {
        delete App.modelProgress[name];
        refreshModels();
      }
    }).catch(() => {
      App.modelProgress[name] = { model: name, pct: 0, done: false, error: "Download failed" };
      updateModelRow(name);
    });
  }

  function cancelDownload(name) {
    Promise.resolve(api.cancel_download && api.cancel_download(name)).catch(() => {});
    delete App.modelProgress[name];
    const m = App.models.find((x) => x.name === name);
    if (m) m.downloading = false;
    updateModelRow(name);
  }

  /* ════════════════════════════════════════════════════════════════════
     SHORTCUTS
     ════════════════════════════════════════════════════════════════════ */
  const SHORTCUTS = [
    { id: "dictate", name: "Dictate" },
    { id: "paste", name: "Paste at cursor" },
    { id: "panel", name: "Open panel" },
    { id: "cancel", name: "Cancel recording" }
  ];
  const scCard = $("shortcutsCard");
  let listeningRow = null;
  function comboHtml(keys) {
    return (keys || []).map((k) => {
      const s = document.createElement("span");
      s.className = "kbd sm";
      s.textContent = k;
      return s.outerHTML;
    }).join("");
  }
  function renderShortcuts() {
    stopListening();
    scCard.innerHTML = "";
    SHORTCUTS.forEach((sc) => {
      const combo = App.shortcuts[sc.id] || { keys: [] };
      const row = document.createElement("div");
      row.className = "row srow";
      row.dataset.id = sc.id;
      row.innerHTML =
        '<div class="lbl"><div class="name"></div><div class="err"></div></div>' +
        '<span class="scombo">' + comboHtml(combo.keys) + "</span>" +
        '<span class="listen"><span class="prompt">Press keys…</span></span>' +
        '<button class="tbtn">Edit</button>';
      row.querySelector(".name").textContent = sc.name;
      const btn = row.querySelector(".tbtn");
      btn.addEventListener("click", () => {
        if (row.classList.contains("listening")) stopListening();
        else startListening(row);
      });
      scCard.appendChild(row);
    });
  }
  function startListening(row) {
    stopListening();
    clearFailed();
    listeningRow = row;
    row.classList.add("listening");
    row.querySelector(".tbtn").textContent = "Cancel";
  }
  function stopListening() {
    if (!listeningRow) return;
    listeningRow.classList.remove("listening");
    listeningRow.querySelector(".tbtn").textContent = "Edit";
    listeningRow = null;
  }
  let failTimer;
  function clearFailed() {
    clearTimeout(failTimer);
    document.querySelectorAll(".srow.failed").forEach((r) => r.classList.remove("failed"));
  }
  function failRebind(row, keys) {
    stopListening();
    clearFailed();
    row.classList.add("failed");
    row.querySelector(".err").textContent =
      "Couldn’t claim " + keys.join(" ") + " — it’s in use elsewhere.";
    failTimer = setTimeout(() => row.classList.remove("failed"), 3200);
  }
  function acceptRebind(row, combo) {
    App.shortcuts[row.dataset.id] = combo;
    stopListening();
    clearFailed();
    row.querySelector(".scombo").innerHTML = comboHtml(combo.keys);
    if (row.dataset.id === "dictate") renderEmptyKeys();
  }
  document.addEventListener("keydown", (e) => {
    if (!listeningRow) return;
    e.preventDefault();
    e.stopPropagation();
    if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return; // wait for a full chord
    const combo = {
      ctrl: e.ctrlKey, alt: e.altKey, shift: e.shiftKey, meta: e.metaKey,
      code: e.code, keys: comboKeys(e)
    };
    const row = listeningRow;
    Promise.resolve(api.rebind_shortcut(row.dataset.id, combo)).then((res) => {
      if (res && res.ok === false) { failRebind(row, combo.keys); return; }
      if (res && res.keys) combo.keys = res.keys;
      acceptRebind(row, combo);
    }).catch(() => failRebind(row, combo.keys));
  }, true);

  function renderEmptyKeys() {
    const box = $("emptyKeys");
    box.innerHTML = "";
    const keys = (App.shortcuts.dictate && App.shortcuts.dictate.keys) || ["Ctrl", "Alt", "Space"];
    keys.forEach((k) => box.appendChild(h("span", { class: "kbd", text: k })));
  }

  /* ════════════════════════════════════════════════════════════════════
     VOCABULARY
     ════════════════════════════════════════════════════════════════════ */
  function persistVocab() {
    Promise.resolve(api.set_vocab && api.set_vocab(App.vocab.hotwords, App.vocab.corrections)).catch(() => {});
  }
  function loadVocab() {
    Promise.resolve(api.list_vocab && api.list_vocab()).then((v) => {
      if (!v) return;
      App.vocab = {
        hotwords: Array.isArray(v.hotwords) ? v.hotwords : [],
        corrections: Array.isArray(v.corrections) ? v.corrections : []
      };
      renderHw();
      renderCorr();
    }).catch(() => {});
  }
  function renderHw(newIdx) {
    const box = $("hwChips");
    box.innerHTML = "";
    App.vocab.hotwords.forEach((w, i) => {
      const c = document.createElement("span");
      c.className = "chip" + (i === newIdx ? " landing" : "");
      c.innerHTML = '<span></span><button class="x" title="Remove">' + xSvg + "</button>";
      c.querySelector("span").textContent = w;
      c.querySelector(".x").addEventListener("click", () => {
        App.vocab.hotwords.splice(i, 1);
        persistVocab();
        renderHw();
      });
      box.appendChild(c);
    });
    box.style.display = App.vocab.hotwords.length ? "" : "none";
    $("hwEmpty").style.display = App.vocab.hotwords.length ? "none" : "block";
  }
  function renderCorr(newIdx) {
    const listBox = $("corrList");
    listBox.innerHTML = "";
    App.vocab.corrections.forEach((p, i) => {
      const r = document.createElement("div");
      r.className = "crow" + (i > 0 ? " hair" : "") + (i === newIdx ? " landing" : "");
      r.innerHTML = '<span class="heard"></span>' + arrSvg + '<span class="written"></span>' +
        '<button class="ghost del" title="Delete">' + xSvg + "</button>";
      r.querySelector(".heard").textContent = p[0];
      r.querySelector(".written").textContent = p[1];
      r.querySelector(".del").addEventListener("click", () => {
        App.vocab.corrections.splice(i, 1);
        persistVocab();
        renderCorr();
      });
      listBox.appendChild(r);
    });
    $("corrEmpty").style.display = App.vocab.corrections.length ? "none" : "block";
  }
  /* hot word add */
  const hwInput = $("hwInput"), hwAdd = $("hwAdd");
  hwInput.addEventListener("input", () => hwAdd.classList.toggle("hasq", !!hwInput.value.trim()));
  hwInput.addEventListener("keydown", (e) => {
    if (e.key !== "Enter") return;
    const w = hwInput.value.trim();
    if (!w) return;
    if (!App.vocab.hotwords.some((x) => x.toLowerCase() === w.toLowerCase())) {
      App.vocab.hotwords.push(w);
      persistVocab();
      renderHw(App.vocab.hotwords.length - 1);
    }
    hwInput.value = "";
    hwAdd.classList.remove("hasq");
  });
  /* correction add */
  const cHeard = $("cHeard"), cWritten = $("cWritten"), cAddBtn = $("cAddBtn");
  function corrReady() { return !!(cHeard.value.trim() && cWritten.value.trim()); }
  function syncCorrBtn() { cAddBtn.disabled = !corrReady(); }
  function commitCorr() {
    App.vocab.corrections.push([cHeard.value.trim(), cWritten.value.trim()]);
    persistVocab();
    renderCorr(App.vocab.corrections.length - 1);
    cHeard.value = ""; cWritten.value = "";
    syncCorrBtn();
    cHeard.focus();
  }
  [cHeard, cWritten].forEach((el) => {
    el.addEventListener("input", syncCorrBtn);
    el.addEventListener("keydown", (e) => {
      if (e.key !== "Enter") return;
      if (corrReady()) commitCorr();
      else if (el === cHeard && cHeard.value.trim()) cWritten.focus();
    });
  });
  cAddBtn.addEventListener("click", commitCorr);

  /* ════════════════════════════════════════════════════════════════════
     RECORD BUTTONS
     ════════════════════════════════════════════════════════════════════ */
  function onRecord() { Promise.resolve(api.toggle_record()).catch(() => {}); }
  $("recBtn").addEventListener("click", onRecord);
  $("recBtn2").addEventListener("click", onRecord);

  /* ════════════════════════════════════════════════════════════════════
     INPUT LEVEL PREVIEW + TEST (design-native webview audio; inside the
     real app getUserMedia is not attempted — the meter breathes idle)
     ════════════════════════════════════════════════════════════════════ */
  let audioCtx = null, analyser = null, micStream = null, micTried = false;
  let gainVal = 0.75, testing = false, monitorGain = null;
  const waveCanvas = $("waveCanvas");
  const wctx = waveCanvas.getContext("2d");
  async function ensureMic() {
    if (micTried) return;
    micTried = true;
    await restartMic();
  }
  async function restartMic() {
    if (!micTried) return;
    stopMonitor();
    if (micStream) { micStream.getTracks().forEach((t) => t.stop()); micStream = null; analyser = null; }
    if (bridgeReady()) return; /* no webview mic grabs inside the app */
    try {
      micStream = await navigator.mediaDevices.getUserMedia({ audio: true });
      audioCtx = audioCtx || new (window.AudioContext || window.webkitAudioContext)();
      if (audioCtx.state === "suspended") audioCtx.resume();
      const src = audioCtx.createMediaStreamSource(micStream);
      analyser = audioCtx.createAnalyser();
      analyser.fftSize = 256;
      src.connect(analyser);
    } catch (_) { analyser = null; } /* no permission — idle simulation */
  }
  /* always-running waveform (functional status motion — kept under reduced motion) */
  const NBARS = 26, bars = new Array(NBARS).fill(0.08);
  let simPhase = 0;
  function sampleLevel() {
    if (analyser) {
      const d = new Uint8Array(analyser.fftSize);
      analyser.getByteTimeDomainData(d);
      let sum = 0;
      for (let i = 0; i < d.length; i++) { const v = (d[i] - 128) / 128; sum += v * v; }
      return Math.min(1, Math.sqrt(sum / d.length) * 3.2);
    }
    simPhase += 0.045;
    return 0.06 + Math.abs(Math.sin(simPhase * 0.7)) * 0.05 + Math.random() * 0.03;
  }
  function drawWave() {
    const lvl = sampleLevel() * gainVal * 1.33;
    bars.pop(); bars.unshift(lvl);
    const db = 20 * Math.log10(Math.max(0.001, Math.min(1, lvl)));
    $("levelVal").textContent = (db <= -60 ? "−∞" : Math.round(db)) + " dB";
    const w = waveCanvas.width, hgt = waveCanvas.height;
    const ink = getComputedStyle(document.body).getPropertyValue("--ink").trim() || "255,255,255";
    wctx.clearRect(0, 0, w, hgt);
    const bw = 5, gap = (w - NBARS * bw) / (NBARS - 1);
    for (let i = 0; i < NBARS; i++) {
      const v = Math.max(0.06, Math.min(1, bars[i]));
      const bh = Math.max(3, v * (hgt - 6));
      const a = testing ? .9 : (.28 + v * .6);
      wctx.fillStyle = "rgba(" + ink + "," + a.toFixed(2) + ")";
      const x = i * (bw + gap), y = (hgt - bh) / 2;
      wctx.beginPath();
      wctx.roundRect(x, y, bw, bh, 2.5);
      wctx.fill();
    }
    requestAnimationFrame(drawWave);
  }
  requestAnimationFrame(drawWave);
  /* input volume (webview preview gain only) */
  const rngGain = $("rngGain");
  rngGain.addEventListener("input", () => {
    gainVal = rngGain.value / 100;
    $("gainVal").textContent = rngGain.value + "%";
    if (monitorGain) monitorGain.gain.value = gainVal;
  });
  /* test: toggle live echo of the mic through the active output */
  const testBtn = $("testBtn");
  function stopMonitor() {
    if (monitorGain) { try { monitorGain.disconnect(); } catch (_) { /* noop */ } monitorGain = null; }
    testing = false;
    testBtn.textContent = "Test";
  }
  testBtn.addEventListener("click", async () => {
    if (testing) { stopMonitor(); return; }
    await ensureMic();
    if (!micStream) await restartMic();
    if (micStream && audioCtx) {
      const src = audioCtx.createMediaStreamSource(micStream);
      monitorGain = audioCtx.createGain();
      monitorGain.gain.value = gainVal;
      src.connect(monitorGain);
      monitorGain.connect(audioCtx.destination);
    }
    testing = true;
    testBtn.textContent = "Stop";
  });

  /* ════════════════════════════════════════════════════════════════════
     STORAGE
     ════════════════════════════════════════════════════════════════════ */
  $("changePathBtn").addEventListener("click", () => {
    Promise.resolve(api.pick_folder()).then((res) => {
      if (res && res.path) {
        App.settings.savePath = res.path;
        $("savePath").textContent = res.path;
      }
    }).catch(() => {});
  });
  function syncStorage() {
    $("savePath").textContent = App.settings.savePath || "";
    const fallback = !!App.settings.storageFallback;
    document.body.classList.toggle("fallback", fallback);
    if (fallback) $("fallbackPath").textContent = App.settings.storagePath || "";
  }

  /* ════════════════════════════════════════════════════════════════════
     STATE INGEST + FULL SYNC
     ════════════════════════════════════════════════════════════════════ */
  function syncSettings() {
    const s = App.settings;
    segSet(document.querySelector('[data-seg="power"]'), s.powerMode || "auto");
    setSw($("swCues"), s.soundCues);
    $("rowVolume").classList.toggle("disabled", !s.soundCues);
    if (typeof s.volume === "number") {
      rngVol.value = s.volume; syncFill(rngVol);
      $("volVal").textContent = s.volume + "%";
    }
    setSw($("swPill"), s.recordingPill);
    segSet(document.querySelector('[data-seg="pillpos"]'), s.pillPosition === "top" ? "top" : "bottom");
    if (typeof s.pillPadding === "number") {
      rngDist.max = String(Math.max(400, s.pillPadding));
      rngDist.value = s.pillPadding; syncFill(rngDist);
      $("distVal").textContent = s.pillPadding + " px";
    }
    syncPillRows();
    segSet(document.querySelector('[data-seg="cleanup"]'), s.clipboardCleanup || "light");
    cleanupSub.textContent = cleanupCopy[s.clipboardCleanup] || cleanupCopy.light;
    setSw($("swVocab"), s.smartVocab);
    setSw($("swSave"), s.saveTranscripts !== false);
    setSw($("swLogin"), s.launchAtLogin);
    syncThemeSeg();
    if (typeof s.transparency === "number") {
      const a = tToAlpha(s.transparency);
      rngGlass.value = a.toFixed(2); syncFill(rngGlass);
      $("glassVal").textContent = a.toFixed(2);
    }
    syncStorage();
    syncModelSelects();
  }

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

  function renderAll() {
    renderEntries();
    renderEmptyKeys();
    renderMics();
    renderShortcuts();
    syncSettings();
    updateEngine();
    if (App.adv && App.view === "history") renderAdv();
  }

  /* ════════════════════════════════════════════════════════════════════
     PUBLIC BACKEND-FACING FUNCTIONS (the contract)
     ════════════════════════════════════════════════════════════════════ */
  window.tiroApplyState = function (state) {
    ingestState(state);
    renderAll();
  };

  window.tiroAddEntry = function (entry) {
    if (!entry) return;
    App.entries.unshift(entry);
    panel.classList.remove("is-empty");
    listEl.prepend(makeEntry(entry, true));
    if (App.adv && App.view === "history" && !searchInput.value.trim()) {
      const body = advList.querySelector('.dayseg[data-day="' + todayIso() + '"] .daybody');
      if (body) {
        const ph = body.querySelector(".dayempty");
        if (ph) ph.remove();
        body.prepend(makeEntry(entry, true));
      }
    }
  };

  window.tiroSetEngine = function (engine) {
    if (!engine) return;
    App.engine = engine;
    updateEngine();
  };

  window.tiroSetTheme = function (effective) {
    applyTheme(effective);
  };

  window.tiroSetRecording = function (on) {
    App.recording = !!on;
    panel.classList.toggle("recording", App.recording);
    $("recBtn").title = App.recording ? "Stop" : "Record";
    $("recBtn2").title = App.recording ? "Stop" : "Record";
  };

  window.tiroSetStorage = function (obj) {
    if (!obj) return;
    App.settings.storageFallback = !!obj.fallback;
    if (typeof obj.path === "string") App.settings.storagePath = obj.path;
    syncStorage();
  };

  window.tiroModelProgress = function (p) {
    if (!p || !p.model) return;
    if (p.done) {
      delete App.modelProgress[p.model];
      const m = App.models.find((x) => x.name === p.model);
      if (m) { m.installed = true; m.downloading = false; }
      refreshModels();
      return;
    }
    if (p.cancelled) {
      delete App.modelProgress[p.model];
      const m = App.models.find((x) => x.name === p.model);
      if (m) m.downloading = false;
      updateModelRow(p.model);
      return;
    }
    App.modelProgress[p.model] = p;
    updateModelRow(p.model);
  };

  /* ════════════════════════════════════════════════════════════════════
     BOOT
     ════════════════════════════════════════════════════════════════════ */
  let _booted = false;
  function boot() {
    if (_booted) return;
    _booted = true;
    if (bridgeReady()) api = window.pywebview.api;
    Promise.resolve(api.get_state()).then((state) => {
      ingestState(state);
      renderAll();
      refreshModels();
      loadVocab();
      loadDays();
    }).catch(() => {
      ingestState(MOCK_STATE);
      renderAll();
    });
  }

  function renderSkeleton() {
    ingestState(MOCK_STATE);
    renderAll();
  }

  if (bridgeReady()) {
    boot();
  } else {
    window.addEventListener("pywebviewready", boot);
    renderSkeleton();
    setTimeout(function () { if (!_booted) boot(); }, 1500);
  }

})();
