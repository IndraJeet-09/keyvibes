/* KeyVibes site — keyboard preview and release lookup. No dependencies. */
(function () {
  "use strict";

  // ------------------------------------------------------------ audio

  var ctx = null;

  function audio() {
    if (!ctx) {
      var AudioContext = window.AudioContext || window.webkitAudioContext;
      if (!AudioContext) return null;
      ctx = new AudioContext();
    }
    if (ctx.state === "suspended") ctx.resume();
    return ctx;
  }

  // A press is a short filtered noise burst over a decaying body tone.
  // Nothing here is a recording: it is a rough stand-in for the real packs.
  function press(voices) {
    var ac = audio();
    if (!ac) return;

    var now = ac.currentTime;
    var cfg = voices || { hp: 1400, body: 170, decay: 0.09, tone: 0.35 };

    var frames = Math.max(1, Math.floor(ac.sampleRate * cfg.decay));
    var buffer = ac.createBuffer(1, frames, ac.sampleRate);
    var data = buffer.getChannelData(0);
    for (var i = 0; i < frames; i++) {
      var t = i / frames;
      data[i] = (Math.random() * 2 - 1) * Math.pow(1 - t, 3);
    }

    var noise = ac.createBufferSource();
    noise.buffer = buffer;

    var band = ac.createBiquadFilter();
    band.type = "bandpass";
    band.frequency.value = cfg.hp;
    band.Q.value = 0.9;

    var body = ac.createOscillator();
    body.type = "triangle";
    body.frequency.setValueAtTime(cfg.body, now);
    body.frequency.exponentialRampToValueAtTime(cfg.body * 0.55, now + cfg.decay);

    var bodyGain = ac.createGain();
    bodyGain.gain.setValueAtTime(cfg.tone, now);
    bodyGain.gain.exponentialRampToValueAtTime(0.0001, now + cfg.decay);

    var out = ac.createGain();
    out.gain.value = 0.5;

    noise.connect(band).connect(out);
    body.connect(bodyGain).connect(out);
    out.connect(ac.destination);

    noise.start(now);
    body.start(now);
    body.stop(now + cfg.decay + 0.02);
  }

  var PACK_VOICES = [
    { hp: 1200, body: 150, decay: 0.11, tone: 0.30 }, // Default — linear
    { hp: 2100, body: 230, decay: 0.08, tone: 0.45 }, // Holy Panda — tactile
    { hp: 3200, body: 340, decay: 0.14, tone: 0.60 }  // Buckling spring
  ];

  // ------------------------------------------------------- key preview

  var keyRow = document.getElementById("keys");
  if (keyRow) {
    var keyButtons = keyRow.querySelectorAll(".key");

    Array.prototype.forEach.call(keyButtons, function (button) {
      button.addEventListener("click", function () {
        button.classList.add("down");
        window.setTimeout(function () { button.classList.remove("down"); }, 90);
        press();
      });
    });

    // Typing on the physical keyboard lights the matching key and plays it,
    // so the page feels like the app does.
    document.addEventListener("keydown", function (event) {
      if (event.metaKey || event.ctrlKey || event.altKey) return;
      var wanted = event.key.toUpperCase();
      Array.prototype.forEach.call(keyButtons, function (button) {
        if (button.textContent.trim().charAt(0) !== wanted) return;
        button.classList.add("down");
        press();
      });
    });
    document.addEventListener("keyup", function (event) {
      var wanted = event.key.toUpperCase();
      Array.prototype.forEach.call(keyButtons, function (button) {
        if (button.textContent.trim().charAt(0) === wanted) {
          button.classList.remove("down");
        }
      });
    });
  }

  // --------------------------------------------------------- pack demo

  Array.prototype.forEach.call(document.querySelectorAll(".pack button"), function (button) {
    button.addEventListener("click", function () {
      var index = parseInt(button.getAttribute("data-press"), 10) || 0;
      var voice = PACK_VOICES[index % PACK_VOICES.length];
      press(voice);
      window.setTimeout(function () { press(voice); }, 160);
    });
  });

  // --------------------------------------------------------- releases

  // `releases.json` is produced by `cargo xtask release` and copied next to
  // these pages. Absent locally, the hardcoded version stays.
  function applyRelease(release) {
    if (!release || !release.version) return;
    var badge = document.getElementById("version-badge");
    if (badge) badge.textContent = "v" + release.version + " · stable";

    Array.prototype.forEach.call(document.querySelectorAll("[data-file]"), function (cell) {
      var file = release.files && release.files[cell.getAttribute("data-file")];
      if (file) cell.textContent = file;
    });
  }

  fetch("releases.json", { cache: "no-cache" })
    .then(function (response) { return response.ok ? response.json() : null; })
    .then(applyRelease)
    .catch(function () { /* static preview without a release yet */ });
})();
