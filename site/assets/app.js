/* KeyVibes site — sound-pack catalog, real pack previews, 3D hero keyboard.
 *
 * Everything the pack list shows comes from packs.json, and every preview
 * comes from the WAV strips `cargo xtask site` exports straight out of the
 * shipped .kvpack files (one file per pack, with the offset of each key's
 * clip recorded beside it). Nothing is synthesized in the browser: if a pack
 * has no clip for a key, that key stays silent.
 *
 * No dependencies, no build step, no autoplay — audio only ever starts from
 * a key press or a click.
 */
(function () {
  "use strict";

  var packs = [];        // every pack in the catalog
  var selected = null;   // pack the hero keyboard plays
  var filter = "all";
  var buffers = {};      // slug -> AudioBuffer
  var loading = {};      // slug -> Promise<AudioBuffer>
  var preview = null;    // the burst currently playing, if any
  var ctx = null;

  /// How long a preview burst runs. Long enough to judge a switch by, short
  /// enough that the page never gets in the way.
  var BURST_SECONDS = 3.6;

  // ------------------------------------------------------------ element ids

  var gridEl = document.getElementById("pack-grid");
  var countEl = document.getElementById("packs-count");
  var provenanceEl = document.getElementById("provenance-list");
  var statusEl = document.getElementById("preview-status");
  var versionEl = document.getElementById("version-badge");
  var heroHearEl = document.getElementById("hero-hear");
  var heroHearLabel = document.getElementById("hero-hear-label");
  var stagePickName = document.getElementById("stage-pick-name");
  var stagePickCap = document.getElementById("stage-pick-cap");

  // ------------------------------------------------------------------ audio

  function audio() {
    if (!ctx) {
      var AudioContext = window.AudioContext || window.webkitAudioContext;
      if (!AudioContext) return null;
      ctx = new AudioContext();
    }
    if (ctx.state === "suspended") ctx.resume();
    return ctx;
  }

  function bufferFor(pack) {
    if (!pack.preview) return Promise.reject(new Error("pack has no preview"));
    if (!buffers[pack.slug]) {
      if (!loading[pack.slug]) {
        loading[pack.slug] = fetch(pack.preview.file, { cache: "force-cache" })
          .then(function (response) {
            if (!response.ok) throw new Error("HTTP " + response.status);
            return response.arrayBuffer();
          })
          .then(function (data) {
            return audio().decodeAudioData(data);
          })
          .then(function (buffer) {
            buffers[pack.slug] = buffer;
            delete loading[pack.slug];
            return buffer;
          })
          .catch(function (error) {
            delete loading[pack.slug];
            throw error;
          });
      }
      return loading[pack.slug];
    }
    return Promise.resolve(buffers[pack.slug]);
  }

  function slotFor(pack, kind, key, exact) {
    if (!pack || !pack.preview) return null;
    var slots = pack.preview.slots;
    var fallback = null;
    for (var i = 0; i < slots.length; i++) {
      var slot = slots[i];
      if (slot.kind !== kind) continue;
      if (slot.key === key) return slot;
      if (!exact && !fallback) fallback = slot;
    }
    return fallback;
  }

  /// One key of the hero keyboard: press and, when the pack has one, the
  /// matching key-up a beat later.
  function playKey(pack, kind, key) {
    if (!pack) return Promise.resolve(false);
    var slot = slotFor(pack, kind, key, true);
    if (!slot) return Promise.resolve(false);
    var ac = audio();
    if (!ac) return Promise.reject(new Error("this browser has no WebAudio"));

    return bufferFor(pack).then(function (buffer) {
      var source = ac.createBufferSource();
      source.buffer = buffer;
      source.connect(ac.destination);
      source.start(ac.currentTime, slot.start, slot.duration);
      return true;
    });
  }

  // ------------------------------------------------------------ burst engine

  /// A small, fast PRNG. The sequence is seeded from the pack's own slug, so
  /// a pack always previews the same way and two packs never preview alike.
  function seeded(seed) {
    var a = seed >>> 0;
    return function () {
      a = (a + 0x6d2b79f5) >>> 0;
      var t = Math.imul(a ^ (a >>> 15), 1 | a);
      t = (t + Math.imul(t ^ (t >>> 7), 61 | t)) ^ t;
      return ((t ^ (t >>> 14)) >>> 0) / 4294967296;
    };
  }

  function hash(text) {
    var h = 2166136261;
    for (var i = 0; i < text.length; i++) {
      h ^= text.charCodeAt(i);
      h = Math.imul(h, 16777619);
    }
    return h >>> 0;
  }

  /// Plans a burst of typing out of the pack's own clips.
  ///
  /// Every sound in the plan is a real press or release clip of this pack —
  /// nothing is borrowed from another key or another pack. The shape of the
  /// plan is a run of short "words": one to three different keys, sometimes
  /// with one of them doubled the way a repeated letter is, separated by
  /// the small gap a typist leaves between words. The timing jitters on
  /// every step, so the burst never turns into a metronome.
  function planBurst(pack) {
    var press = [];
    var release = {};
    var slots = pack.preview.slots;
    for (var i = 0; i < slots.length; i++) {
      if (slots[i].kind === "press") press.push(slots[i]);
      else release[slots[i].key] = slots[i];
    }
    if (!press.length) return null;

    var rand = seeded(hash(pack.slug));
    var events = [];
    var t = 0;

    while (t < BURST_SECONDS) {
      var letters = 1 + Math.floor(rand() * 3);
      var word = [];
      for (var l = 0; l < letters; l++) {
        word.push(press[Math.floor(rand() * press.length)]);
      }
      // Double one letter, the way "ll" or "ee" repeats in a real word.
      if (word.length > 1 && rand() < 0.4) {
        var at = Math.floor(rand() * word.length);
        word.splice(at, 0, word[Math.floor(rand() * word.length)]);
      }

      for (var w = 0; w < word.length && t < BURST_SECONDS; w++) {
        var slot = word[w];
        events.push({ slot: slot, at: t, gain: 0.84 + rand() * 0.16 });

        var up = release[slot.key];
        if (up && rand() < 0.45) {
          events.push({ slot: up, at: t + 0.03 + rand() * 0.05, gain: 0.45 + rand() * 0.2 });
        }
        // Key travel: quick, but never metronomic.
        t += 0.042 + rand() * 0.062;
      }
      // The pause between words.
      t += 0.07 + rand() * 0.15;
    }

    return { events: events, end: t };
  }

  /// Lays the planned burst onto any audio context: the live one for
  /// playback, an offline one for the verification hook.
  function scheduleBurst(ac, buffer, plan, startAt) {
    var bus = ac.createGain();
    bus.gain.value = 1;
    bus.connect(ac.destination);

    var sources = [];
    plan.events.forEach(function (event) {
      var source = ac.createBufferSource();
      source.buffer = buffer;
      var gain = ac.createGain();
      gain.gain.value = event.gain;
      source.connect(gain);
      gain.connect(bus);
      source.start(startAt + event.at, event.slot.start, event.slot.duration);
      sources.push(source);
    });
    return { bus: bus, sources: sources };
  }

  /// Plays a pack's burst. Any preview already running is stopped first, so
  /// two bursts can never overlap.
  function playBurst(pack) {
    stopPreview();
    if (!pack) return Promise.reject(new Error("no pack selected"));
    var ac = audio();
    if (!ac) return Promise.reject(new Error("this browser has no WebAudio"));
    var plan = planBurst(pack);
    if (!plan) return Promise.reject(new Error("this pack has no playable clip"));

    return bufferFor(pack).then(function (buffer) {
      var start = ac.currentTime + 0.05;
      var scheduled = scheduleBurst(ac, buffer, plan, start);

      preview = {
        pack: pack,
        bus: scheduled.bus,
        sources: scheduled.sources,
        timer: 0
      };
      markPlaying(pack);

      // Let the last clip decay before the state clears.
      preview.timer = window.setTimeout(function () {
        stopPreview();
      }, (plan.end + 0.35) * 1000);
      return true;
    });
  }

  /// Stops the running preview immediately and releases its audio nodes.
  function stopPreview() {
    if (!preview) return;
    window.clearTimeout(preview.timer);
    preview.sources.forEach(function (source) {
      try { source.stop(); } catch (e) { /* already ended */ }
      try { source.disconnect(); } catch (e) { /* already detached */ }
    });
    try { preview.bus.disconnect(); } catch (e) { /* already detached */ }
    preview = null;
    markPlaying(null);
  }

  function markPlaying(pack) {
    if (gridEl) {
      Array.prototype.forEach.call(gridEl.querySelectorAll(".pack-card"), function (card) {
        var on = !!pack && card.getAttribute("data-slug") === pack.slug;
        card.classList.toggle("is-playing", on);
        var button = card.querySelector(".pack-card__button");
        if (button) button.setAttribute("aria-pressed", on ? "true" : "false");
      });
    }
    if (heroHearEl) {
      heroHearEl.setAttribute("aria-pressed", pack ? "true" : "false");
      if (heroHearLabel) heroHearLabel.textContent = pack ? "Stop" : "Hear it";
    }
  }

  /// Says what went wrong without leaving the page in a broken state, and
  /// without pretending a missing clip was played.
  function reportAudioError(error) {
    if (preview) stopPreview();
    if (!statusEl) return;
    statusEl.textContent = "That preview could not be played (" + error.message + ").";
    statusEl.classList.add("is-shown");
    window.clearTimeout(reportAudioError.timer);
    reportAudioError.timer = window.setTimeout(function () {
      statusEl.textContent = "";
      statusEl.classList.remove("is-shown");
    }, 6000);
  }

  // ---------------------------------------------------------------- catalog

  /// Illustrative cap colour, keyed by the pack's own switch so a Cherry MX
  /// Black card looks black because the switch is black. This is presentation
  /// only: it says nothing about the recording, and the catalog carries no
  /// colour of its own.
  var CAP_COLORS = {
    "alpaca": "#f2a7c3",
    "buckling-spring": "#cfc6b4",
    "cherry-mx-black-abs": "#2c2c30",
    "cherry-mx-black-pbt": "#34343a",
    "cherry-mx-blue-abs": "#1f6feb",
    "cherry-mx-blue-pbt": "#2a7bf0",
    "cherry-mx-brown-abs": "#8b5a2b",
    "cherry-mx-brown-pbt": "#8f5f30",
    "cherry-mx-red-abs": "#d7263d",
    "cherry-mx-red-pbt": "#d42b42",
    "everglide-crystal-purple": "#a77bf3",
    "everglide-oreo": "#3b3b3b",
    "gateron-black-ink": "#3a3f4a",
    "gateron-red-ink": "#b3203b",
    "holy-panda-kbsim": "#e9a23b",
    "kailh-box-navy": "#1b2f5e",
    "novelkeys-cream": "#ebd9b4",
    "skcm-blue-alps": "#5aa9e6",
    "topre": "#8e8e93",
    "topre-purple-hybrid-pbt": "#7e57c2",
    "turquoise-tealios": "#2ec4b6"
  };

  var CAP_FALLBACK = ["#7c4dff", "#2ec4b6", "#e9a23b", "#a77bf3", "#5aa9e6", "#d7263d", "#8e8e93"];

  function capColor(pack) {
    if (CAP_COLORS[pack.slug]) return CAP_COLORS[pack.slug];
    return CAP_FALLBACK[hash(pack.slug) % CAP_FALLBACK.length];
  }

  function categoryLabel(pack) {
    if (pack.synthesized) return "Demo";
    if (pack.category === "uncategorized") return "Uncategorized";
    return pack.category.charAt(0).toUpperCase() + pack.category.slice(1);
  }

  /// Splits a pack's real name into a display name and a variant badge, so
  /// "Cherry MX Black PBT" reads as "Cherry MX Black" plus a PBT chip. The
  /// name itself is never invented — only split where the pack names a
  /// variant.
  function splitName(name) {
    var variant = /\s(ABS|PBT)$/.exec(name);
    if (variant) {
      return { name: name.slice(0, -variant[0].length), variant: variant[1] };
    }
    var bracket = /\s\(([^()]+)\)$/.exec(name);
    if (bracket) {
      return { name: name.slice(0, -bracket[0].length), variant: bracket[1] };
    }
    return { name: name, variant: null };
  }

  function matchesFilter(pack) {
    if (filter === "all") return true;
    return pack.category === filter;
  }

  function buildCard(pack) {
    var item = document.createElement("li");
    item.className = "pack-card";
    item.setAttribute("data-slug", pack.slug);
    item.setAttribute("data-category", pack.category);

    var button = document.createElement("button");
    button.type = "button";
    button.className = "pack-card__button";
    button.setAttribute("aria-pressed", "false");

    var cap = document.createElement("span");
    cap.className = "keycap";
    cap.style.setProperty("--cap", capColor(pack));
    cap.setAttribute("aria-hidden", "true");

    var text = document.createElement("span");
    text.className = "pack-card__text";

    var parts = splitName(pack.name);
    var name = document.createElement("span");
    name.className = "pack-card__name";
    name.textContent = parts.name;
    if (parts.variant) {
      var chip = document.createElement("span");
      chip.className = "chip";
      chip.textContent = parts.variant;
      name.appendChild(chip);
    }

    var meta = document.createElement("span");
    meta.className = "pack-card__meta";

    var category = document.createElement("span");
    category.className = "meta__part";
    category.textContent = categoryLabel(pack);
    meta.appendChild(category);

    var sep = document.createElement("span");
    sep.className = "sep";
    sep.textContent = "·";
    meta.appendChild(sep);

    var coverage = document.createElement("span");
    coverage.className = "meta__part";
    coverage.textContent = pack.coverage + "/104";
    meta.appendChild(coverage);

    var unit = document.createElement("span");
    unit.className = "meta__part meta__part--optional";
    unit.textContent = " keys";
    meta.appendChild(unit);

    if (pack.has_release) {
      var sep2 = document.createElement("span");
      sep2.className = "sep sep--optional";
      sep2.textContent = "·";
      meta.appendChild(sep2);
      var release = document.createElement("span");
      release.className = "meta__part meta__part--optional";
      release.textContent = "release";
      meta.appendChild(release);
    }

    text.appendChild(name);
    text.appendChild(meta);

    var play = document.createElement("span");
    play.className = "pack-card__play";
    play.setAttribute("aria-hidden", "true");
    play.innerHTML =
      '<svg class="icon-play" viewBox="0 0 24 24" aria-hidden="true"><path d="M8.4 5.6v12.8a.6.6 0 0 0 .93.5l9.6-6.4a.6.6 0 0 0 0-1l-9.6-6.4a.6.6 0 0 0-.93.5z"/></svg>' +
      '<svg class="icon-stop" viewBox="0 0 24 24" aria-hidden="true"><rect x="7" y="7" width="10" height="10" rx="1.6"/></svg>';

    button.appendChild(cap);
    button.appendChild(text);
    button.appendChild(play);

    var title = [pack.name, pack.description, pack.author, pack.license]
      .filter(Boolean)
      .join(" · ");
    button.title = title;

    button.addEventListener("click", function () {
      if (preview && preview.pack && preview.pack.slug === pack.slug) {
        stopPreview();
        return;
      }
      select(pack);
      playBurst(pack).catch(reportAudioError);
    });

    item.appendChild(button);
    return item;
  }

  function render() {
    if (!gridEl) return;
    while (gridEl.firstChild) gridEl.removeChild(gridEl.firstChild);

    var visible = packs.filter(matchesFilter);
    if (!visible.length) {
      var empty = document.createElement("li");
      empty.className = "packs-empty";
      empty.textContent = "No pack in that switch family is installed.";
      gridEl.appendChild(empty);
    } else {
      visible.forEach(function (pack) { gridEl.appendChild(buildCard(pack)); });
    }

    if (countEl) {
      var withRelease = packs.filter(function (pack) { return pack.has_release; }).length;
      countEl.textContent = filter === "all"
        ? packs.length + " packs · " + withRelease + " with release sounds · 3–4s previews"
        : visible.length + " of " + packs.length + " packs · " + withRelease + " with release sounds";
    }
    markSelected();
  }

  function markSelected() {
    if (!gridEl) return;
    Array.prototype.forEach.call(gridEl.querySelectorAll(".pack-card"), function (card) {
      var on = !!selected && card.getAttribute("data-slug") === selected.slug;
      card.classList.toggle("is-selected", on);
    });
  }

  function select(pack) {
    selected = pack;
    if (stagePickName) stagePickName.textContent = pack.name;
    if (stagePickCap) stagePickCap.style.setProperty("--cap", capColor(pack));
    if (statusEl) {
      statusEl.textContent = "";
      statusEl.classList.remove("is-shown");
    }
    markSelected();
    markCoveredKeys(pack);
  }

  /// Caps the pack has no clip for are dimmed and labelled, so "this key is
  /// silent in this pack" is visible rather than discovered by pressing it.
  function markCoveredKeys(pack) {
    if (!capButtons.length) return;
    capButtons.forEach(function (button) {
      var key = button.getAttribute("data-key");
      var covered = !!slotFor(pack, "press", key, true);
      button.classList.toggle("is-missing", !covered);
      button.title = covered
        ? ""
        : pack.name + " has no clip for " + key + ", so it stays silent";
    });
  }

  function buildProvenance() {
    if (!provenanceEl) return;
    while (provenanceEl.firstChild) provenanceEl.removeChild(provenanceEl.firstChild);
    packs.forEach(function (pack) {
      var item = document.createElement("li");
      var name = document.createElement("b");
      name.textContent = pack.name;
      item.appendChild(name);
      var line = [pack.author, pack.license, pack.source].filter(Boolean).join(" · ");
      item.appendChild(document.createTextNode(line));
      provenanceEl.appendChild(item);
    });
  }

  function withCatalog() {
    var start = packs[0];
    if (start) select(start);
    render();
    buildProvenance();

    Array.prototype.forEach.call(document.querySelectorAll(".segmented button"), function (chip) {
      chip.addEventListener("click", function () {
        filter = chip.getAttribute("data-filter") || "all";
        Array.prototype.forEach.call(document.querySelectorAll(".segmented button"), function (other) {
          var on = other === chip;
          other.setAttribute("aria-pressed", on ? "true" : "false");
        });
        render();
      });
    });
  }

  function catalogFailed(error) {
    if (gridEl) {
      while (gridEl.firstChild) gridEl.removeChild(gridEl.firstChild);
      var status = document.createElement("li");
      status.className = "packs-empty";
      status.textContent =
        "The pack catalog could not be read (" + error.message +
        "). Generate it with `cargo xtask site`, which writes site/packs.json " +
        "and the preview audio beside it.";
      gridEl.appendChild(status);
    }
    if (countEl) countEl.textContent = "catalog unavailable";
  }

  // ----------------------------------------------------------- hero keyboard

  /// The board: a TKL in layout order. `u` is width in key units, `code` is
  /// the pack's physical key name, `label` is what the cap shows.
  var F = function (code, label, u, accent) {
    return { code: code, label: label, u: u || 1, accent: !!accent };
  };

  var KEYBOARD_ROWS = [
    {
      spread: true,
      left: [
        F("Escape", "Esc", 1, true),
        F("F1", "F1"), F("F2", "F2"), F("F3", "F3"), F("F4", "F4"),
        F("F5", "F5"), F("F6", "F6"), F("F7", "F7"), F("F8", "F8"),
        F("F9", "F9"), F("F10", "F10"), F("F11", "F11"), F("F12", "F12")
      ],
      right: [F("PrintScreen", "Prt"), F("ScrollLock", "Scr"), F("Pause", "Pse")]
    },
    {
      left: [
        F("Grave", "`"), F("Digit1", "1"), F("Digit2", "2"), F("Digit3", "3"),
        F("Digit4", "4"), F("Digit5", "5"), F("Digit6", "6"), F("Digit7", "7"),
        F("Digit8", "8"), F("Digit9", "9"), F("Digit0", "0"),
        F("Minus", "-"), F("Equal", "="), F("Backspace", "⌫", 2)
      ],
      right: [F("Insert", "Ins"), F("Home", "Hom"), F("PageUp", "PgU")]
    },
    {
      left: [
        F("Tab", "Tab", 1.5),
        F("Q", "Q"), F("W", "W"), F("E", "E"), F("R", "R"), F("T", "T"),
        F("Y", "Y"), F("U", "U"), F("I", "I"), F("O", "O"), F("P", "P"),
        F("LeftBracket", "["), F("RightBracket", "]"),
        F("Backslash", "\\", 1.5)
      ],
      right: [F("Delete", "Del"), F("End", "End"), F("PageDown", "PgD")]
    },
    {
      left: [
        F("CapsLock", "Caps", 1.75),
        F("A", "A"), F("S", "S"), F("D", "D"), F("F", "F"), F("G", "G"),
        F("H", "H"), F("J", "J"), F("K", "K"), F("L", "L"),
        F("Semicolon", ";"), F("Apostrophe", "'"),
        F("Enter", "Enter", 2.25, true)
      ],
      right: []
    },
    {
      left: [
        F("LeftShift", "Shift", 2.25),
        F("Z", "Z"), F("X", "X"), F("C", "C"), F("V", "V"), F("B", "B"),
        F("N", "N"), F("M", "M"),
        F("Comma", ","), F("Period", "."), F("Slash", "/"),
        F("RightShift", "Shift", 2.75)
      ],
      right: [F("ArrowUp", "↑")]
    },
    {
      left: [
        F("LeftCtrl", "Ctrl", 1.25), F("LeftSuper", "Super", 1.25),
        F("LeftAlt", "Alt", 1.25), F("Space", "", 6.25),
        F("RightAlt", "Alt", 1.25), F("RightSuper", "Super", 1.25),
        F("Menu", "Menu", 1.25), F("RightCtrl", "Ctrl", 1.25)
      ],
      right: [F("ArrowLeft", "←"), F("ArrowDown", "↓"), F("ArrowRight", "→")]
    }
  ];

  var capButtons = [];

  function buildKeyboard() {
    var plate = document.getElementById("kb-plate");
    if (!plate) return;
    while (plate.firstChild) plate.removeChild(plate.firstChild);

    KEYBOARD_ROWS.forEach(function (row) {
      var rowEl = document.createElement("div");
      rowEl.className = "kb__row";

      var left = document.createElement("div");
      left.className = "kb__group kb__group--left" + (row.spread ? " kb__group--spread" : "");
      row.left.forEach(function (key) { left.appendChild(buildCap(key)); });
      rowEl.appendChild(left);

      var right = document.createElement("div");
      right.className = "kb__group kb__group--right" +
        (row.right.length === 1 ? " kb__group--centre" : "");
      row.right.forEach(function (key) { right.appendChild(buildCap(key)); });
      rowEl.appendChild(right);

      plate.appendChild(rowEl);
    });
  }

  function buildCap(key) {
    var button = document.createElement("button");
    button.type = "button";
    button.className = "cap" + (key.accent ? " cap--accent" : "") +
      (key.u > 1 ? " cap--wide" : "");
    button.style.setProperty("--u", key.u);
    button.setAttribute("data-key", key.code);

    var body = document.createElement("span");
    body.className = "cap__body";
    var face = document.createElement("span");
    face.className = "cap__face";
    var legend = document.createElement("span");
    legend.className = "cap__legend";
    legend.textContent = key.label;
    face.appendChild(legend);

    button.appendChild(body);
    button.appendChild(face);

    button.addEventListener("click", function () {
      flashCap(button);
      playKey(selected, "press", key.code)
        .then(function (ok) {
          if (!ok) return;
          return playKey(selected, "release", key.code);
        })
        .catch(reportAudioError);
    });

    capButtons.push(button);
    return button;
  }

  function flashCap(button) {
    button.classList.add("is-down");
    window.setTimeout(function () { button.classList.remove("is-down"); }, 110);
  }

  /// Browser key names to the pack's physical key names.
  var KEY_MAP = {
    " ": "Space",
    "Enter": "Enter",
    "Backspace": "Backspace",
    "Tab": "Tab",
    "Escape": "Escape",
    "Shift": "LeftShift",
    "Control": "LeftCtrl",
    "Alt": "LeftAlt",
    "Meta": "LeftSuper",
    "ArrowUp": "ArrowUp",
    "ArrowDown": "ArrowDown",
    "ArrowLeft": "ArrowLeft",
    "ArrowRight": "ArrowRight",
    "-": "Minus",
    "=": "Equal",
    "[": "LeftBracket",
    "]": "RightBracket",
    "\\": "Backslash",
    ";": "Semicolon",
    "'": "Apostrophe",
    ",": "Comma",
    ".": "Period",
    "/": "Slash",
    "`": "Grave"
  };

  function physicalKeyFor(event) {
    var key = event.key || "";
    if (/^[a-z]$/i.test(key)) return key.toUpperCase();
    if (/^[0-9]$/.test(key)) return "Digit" + key;
    return KEY_MAP[key] || null;
  }

  // -------------------------------------------------------------- reveals

  function revealHeadline() {
    var title = document.getElementById("hero-title");
    if (!title) return;
    title.classList.add("is-split");
    window.requestAnimationFrame(function () {
      window.requestAnimationFrame(function () {
        title.classList.add("is-revealed");
      });
    });
  }

  function revealOnScroll() {
    var targets = document.querySelectorAll("[data-reveal]");
    if (!targets.length) return;

    if (!("IntersectionObserver" in window)) {
      Array.prototype.forEach.call(targets, function (el) { el.classList.add("is-visible"); });
      return;
    }

    var observer = new IntersectionObserver(function (entries) {
      entries.forEach(function (entry) {
        if (!entry.isIntersecting) return;
        entry.target.classList.add("is-visible");
        observer.unobserve(entry.target);
      });
    }, { rootMargin: "0px 0px -12% 0px", threshold: 0.1 });

    Array.prototype.forEach.call(targets, function (el) { observer.observe(el); });
  }

  // --------------------------------------------------------------- catalog

  fetch("packs.json", { cache: "no-cache" })
    .then(function (response) {
      if (!response.ok) throw new Error("HTTP " + response.status);
      return response.json();
    })
    .then(function (catalog) {
      packs = Array.isArray(catalog.packs) ? catalog.packs : [];
      if (!packs.length) throw new Error("the catalog lists no packs");
      withCatalog();
    })
    .catch(catalogFailed);

  // ------------------------------------------------------------- bootstrap

  buildKeyboard();
  revealHeadline();
  revealOnScroll();

  if (heroHearEl) {
    heroHearEl.addEventListener("click", function () {
      if (preview) {
        stopPreview();
        return;
      }
      playBurst(selected).catch(reportAudioError);
    });
  }

  // Typing on the physical keyboard lights the matching cap and plays the
  // selected pack's clip for it, so the page behaves like the app.
  document.addEventListener("keydown", function (event) {
    if (event.repeat || event.metaKey || event.ctrlKey || event.altKey) return;
    var code = physicalKeyFor(event);
    if (!code) return;
    capButtons.forEach(function (button) {
      if (button.getAttribute("data-key") !== code) return;
      flashCap(button);
    });
    playKey(selected, "press", code).catch(reportAudioError);
  });

  document.addEventListener("keyup", function (event) {
    var code = physicalKeyFor(event);
    if (!code) return;
    capButtons.forEach(function (button) {
      if (button.getAttribute("data-key") === code) button.classList.remove("is-down");
    });
  });

  // A preview must not survive leaving the page.
  window.addEventListener("pagehide", function () { stopPreview(); });

  // --------------------------------------------------------------- releases

  /// Where the release assets live. `releases/latest` always points at the
  /// newest published release, so a link built from the file name in
  /// releases.json resolves to the current build rather than to whatever
  /// happened to be published when this page was written.
  var RELEASE_DOWNLOADS =
    "https://github.com/IndraJeet-09/keyvibes/releases/latest/download/";

  /// `releases.json` is produced by `cargo xtask dist` and published with
  /// the release. Absent locally, the checked-in copy stays.
  function applyRelease(release) {
    if (!release || !release.version) return;
    if (versionEl) versionEl.textContent = "v" + release.version;

    var files = release.files || {};

    Array.prototype.forEach.call(document.querySelectorAll("[data-file]"), function (cell) {
      var file = files[cell.getAttribute("data-file")];
      if (file) cell.textContent = file;
    });

    Array.prototype.forEach.call(document.querySelectorAll("[data-download]"), function (link) {
      var file = files[link.getAttribute("data-download")];
      if (file) link.setAttribute("href", RELEASE_DOWNLOADS + file);
    });

    // A row for a format this release does not carry would download a file
    // that was never published. Take the row out instead.
    Array.prototype.forEach.call(document.querySelectorAll("tr[data-kind]"), function (row) {
      row.hidden = !files[row.getAttribute("data-kind")];
    });
  }

  fetch("releases.json", { cache: "no-cache" })
    .then(function (response) { return response.ok ? response.json() : null; })
    .then(applyRelease)
    .catch(function () { /* static preview without a release yet */ });

  /// A cheap fingerprint of a rendered waveform, so two renders can be
  /// compared without shipping the audio back out of the browser.
  function fingerprint(data) {
    var h = 2166136261;
    for (var i = 0; i < data.length; i += 7) {
      h ^= Math.round(data[i] * 32767);
      h = Math.imul(h, 16777619);
    }
    return (h >>> 0).toString(16);
  }

  /// Renders a pack's burst offline and measures it. Nothing is sent to the
  /// speakers: this exists so a test can prove the preview really is the
  /// pack's own audio, and that two packs do not sound alike.
  function renderBurst(pack) {
    var Offline = window.OfflineAudioContext || window.webkitOfflineAudioContext;
    if (!Offline) return Promise.reject(new Error("no offline audio context"));
    var plan = planBurst(pack);
    if (!plan) return Promise.reject(new Error("this pack has no playable clip"));

    return bufferFor(pack).then(function (buffer) {
      var rate = buffer.sampleRate;
      var frames = Math.ceil((plan.end + 0.5) * rate);
      var offline = new Offline(1, frames, rate);
      var copy = offline.createBuffer(
        buffer.numberOfChannels,
        buffer.length,
        rate
      );
      for (var ch = 0; ch < buffer.numberOfChannels; ch++) {
        copy.copyToChannel(buffer.getChannelData(ch), ch);
      }
      scheduleBurst(offline, copy, plan, 0);
      return offline.startRendering().then(function (rendered) {
        var data = rendered.getChannelData(0);
        var peak = 0;
        var sum = 0;
        var first = -1;
        var last = -1;
        for (var i = 0; i < data.length; i++) {
          var v = Math.abs(data[i]);
          if (v > peak) peak = v;
          sum += v * v;
          if (v > 0.002) {
            if (first < 0) first = i;
            last = i;
          }
        }
        return {
          peak: peak,
          rms: Math.sqrt(sum / Math.max(1, data.length)),
          // The span the burst actually covers, first sound to last.
          span: first < 0 ? 0 : (last - first) / rate,
          seconds: rendered.length / rate,
          fingerprint: fingerprint(data)
        };
      });
    });
  }

  // Diagnostic hook for the site's headless smoke test and for anyone
  // poking at the page in devtools. It reads state the page already has;
  // it cannot play, load or change anything on its own.
  Object.defineProperty(window, "__keyvibes", {
    value: {
      packs: function () { return packs.slice(); },
      selected: function () { return selected; },
      filter: function () { return filter; },
      isDecoded: function (slug) { return !!buffers[slug]; },
      isPlaying: function () { return !!preview; },
      playingSlug: function () { return preview ? preview.pack.slug : null; },
      plan: function (slug) {
        var pack = packs.filter(function (p) { return p.slug === slug; })[0];
        if (!pack) return null;
        var plan = planBurst(pack);
        if (!plan) return null;
        return {
          events: plan.events.length,
          seconds: plan.end,
          keys: plan.events.filter(function (e) { return e.slot.kind === "press"; })
            .map(function (e) { return e.slot.key; })
        };
      },
      slotsFor: function (key) {
        if (!selected || !selected.preview) return [];
        return selected.preview.slots.filter(function (slot) { return slot.key === key; });
      },
      /// Renders a pack's burst offline and reports what came out. This
      /// makes no sound on the speakers.
      render: function (slug) {
        var pack = packs.filter(function (p) { return p.slug === slug; })[0];
        if (!pack) return Promise.reject(new Error("no such pack"));
        return renderBurst(pack);
      }
    },
    writable: false
  });
})();
