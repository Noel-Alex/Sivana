// Sivana client: capture -> local fingerprinting -> streaming match.
// Raw audio never leaves the page; only SFP1 fingerprint batches are sent.
// The visualizer reads the same audio via an AnalyserNode tap.

const $ = (id) => document.getElementById(id);
const state = {
  ws: null, session: null, engine: null, analyser: null,
  landmarks: 0, startedAt: 0, stream: null, ctx: null,
  anchors: [], done: false, level: 0, deadlineTimer: null, resampler: null,
};

const CLIENT_LISTENING_LIMIT_MS = 13_000;
const FINGERPRINT_SAMPLE_RATE = 22_050;

// AudioContext's requested sample rate is only a preference on some devices.
// Keep the fingerprint geometry identical to ingestion even when the browser
// delivers microphone PCM at the hardware rate (commonly 44.1 or 48 kHz).
class StreamingLinearResampler {
  constructor(inputRate, outputRate) {
    this.step = inputRate / outputRate;
    this.position = 0;
    this.pending = new Float32Array(0);
  }

  process(input) {
    if (this.step === 1 && this.pending.length === 0) return input;
    const data = new Float32Array(this.pending.length + input.length);
    data.set(this.pending);
    data.set(input, this.pending.length);
    const output = [];
    while (this.position + 1 < data.length) {
      const i = Math.floor(this.position);
      const fraction = this.position - i;
      output.push(data[i] + (data[i + 1] - data[i]) * fraction);
      this.position += this.step;
    }
    const consumed = Math.floor(this.position);
    this.pending = data.slice(consumed);
    this.position -= consumed;
    return Float32Array.from(output);
  }
}

function show(panel) {
  const map = {
    idle: "panel-hero",
    listening: "panel-listening",
    matched: "panel-result",
    "no-match": "panel-nomatch",
  };
  document.body.className = "state-" + panel;
  for (const id of Object.values(map)) document.getElementById(id).classList.add("hidden");
  document.getElementById(map[panel]).classList.remove("hidden");
}

function note(line) {
  const pre = document.getElementById("colo-body");
  pre.textContent += line + "\n";
}

function bindDrawer(btnId, drawerId) {
  const button = document.getElementById(btnId);
  button.addEventListener("click", () => {
    const d = document.getElementById(drawerId);
    d.classList.toggle("open");
    d.setAttribute("aria-hidden", String(!d.classList.contains("open")));
    button.setAttribute("aria-expanded", String(d.classList.contains("open")));
    for (const other of ["drawer-archive", "drawer-notes"]) {
      if (other !== drawerId) {
        const otherDrawer = document.getElementById(other);
        otherDrawer.classList.remove("open");
        otherDrawer.setAttribute("aria-hidden", "true");
        document.querySelector(`[aria-controls="${other}"]`)?.setAttribute("aria-expanded", "false");
      }
    }
  });
}
bindDrawer("toggle-archive", "drawer-archive");
bindDrawer("toggle-notes", "drawer-notes");

document.querySelectorAll("[data-close-drawer]").forEach((button) => {
  button.addEventListener("click", () => {
    const id = button.dataset.closeDrawer;
    const drawer = document.getElementById(id);
    drawer.classList.remove("open");
    drawer.setAttribute("aria-hidden", "true");
    document.querySelector(`[aria-controls="${id}"]`)?.setAttribute("aria-expanded", "false");
  });
});

const viz = document.getElementById("viz");
const vctx = viz.getContext("2d");
let freqData = null;

function resizeViz() {
  const rect = viz.getBoundingClientRect();
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  viz.width = Math.max(1, Math.round(rect.width * dpr));
  viz.height = Math.max(1, Math.round(rect.height * dpr));
}
window.addEventListener("resize", resizeViz);
resizeViz();

const BARS_PER_SIDE = 54;

function drawViz(t) {
  const rect = viz.getBoundingClientRect();
  const dpr = Math.min(window.devicePixelRatio || 1, 2);
  const wantW = Math.max(1, Math.round(rect.width * dpr));
  const wantH = Math.max(1, Math.round(rect.height * dpr));
  if (viz.width !== wantW || viz.height !== wantH) resizeViz();
  const W = viz.width, H = viz.height;
  const cx = W / 2;
  const cy = H / 2;
  const sideWidth = W * 0.485;
  const step = sideWidth / BARS_PER_SIDE;
  const maxHeight = H * 0.42;
  vctx.clearRect(0, 0, W, H);

  const levels = new Float32Array(BARS_PER_SIDE);
  if (state.analyser && freqData) {
    state.analyser.getByteFrequencyData(freqData);
    let sum = 0;
    for (let i = 0; i < BARS_PER_SIDE; i++) {
      const bin = Math.floor(Math.pow(i / BARS_PER_SIDE, 1.7) * freqData.length * 0.82);
      levels[i] = freqData[bin] / 255;
      sum += levels[i];
    }
    state.level = state.level * 0.82 + (sum / BARS_PER_SIDE) * 0.18;
  } else {
    const idleMotion = window.matchMedia("(prefers-reduced-motion: reduce)").matches ? 0 : t;
    for (let i = 0; i < BARS_PER_SIDE; i++) {
      levels[i] = 0.12 + 0.075 * Math.sin(idleMotion / 620 + i * 0.58) + 0.035 * Math.sin(i * 1.7);
    }
  }

  vctx.strokeStyle = "rgba(17,19,15,0.48)";
  vctx.lineWidth = Math.max(1, W / 1800);
  vctx.beginPath();
  vctx.moveTo(0, cy);
  vctx.lineTo(W, cy);
  vctx.stroke();

  for (let i = 0; i < BARS_PER_SIDE; i++) {
    const energy = Math.max(0.025, levels[i]);
    const len = Math.max(2 * dpr, energy * maxHeight);
    const hot = energy > 0.56;
    vctx.strokeStyle = hot ? "#ff5e3a" : energy > 0.24 ? "#2447d8" : "rgba(17,19,15,0.52)";
    vctx.lineWidth = Math.max(1, Math.min(3, step * 0.28));
    for (const direction of [-1, 1]) {
      const x = cx + direction * (i + 0.75) * step;
      const asymmetry = 0.7 + 0.3 * Math.sin(i * 2.31);
      const top = len * (direction === 1 ? 1 : asymmetry);
      const bottom = len * (direction === -1 ? 1 : asymmetry);
      vctx.beginPath();
      vctx.moveTo(x, cy - top);
      vctx.lineTo(x, cy + bottom);
      vctx.stroke();
    }
  }

  for (const blip of state.anchors) {
    const age = (t - blip.t) / 1800;
    if (age > 1) continue;
    const direction = blip.a > Math.PI ? -1 : 1;
    const x = cx + direction * age * sideWidth;
    const size = Math.max(2, W / 700);
    vctx.globalAlpha = (1 - age) * 0.9;
    vctx.fillStyle = "#ff5e3a";
    vctx.fillRect(x - size / 2, cy - size * 1.5, size, size * 3);
  }
  vctx.globalAlpha = 1;

  requestAnimationFrame(drawViz);
}
requestAnimationFrame(drawViz);

async function health() {
  try {
    const r = await fetch("/v1/health");
    const j = await r.json();
    document.getElementById("catalog-version").textContent = "V" + j.catalog_version;
  } catch {
    document.getElementById("catalog-version").textContent = "OFFLINE";
  }
}

async function startListening() {
  note("--- session " + new Date().toISOString().slice(11, 19) + " ---");
  state.landmarks = 0;
  state.anchors = [];
  state.done = false;
  show("listening");
  document.getElementById("fact-status").textContent = "LISTENING";

  const r = await fetch("/v1/sessions", { method: "POST" });
  if (!r.ok) {
    throw new Error("recognition service returned HTTP " + r.status);
  }
  const { session_id } = await r.json();
  state.session = session_id;

  const wsUrl = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/v1/identify/" + session_id;
  state.ws = new WebSocket(wsUrl);
  state.ws.binaryType = "arraybuffer";
  state.ws.onmessage = (e) => {
    const ev = JSON.parse(e.data);
    note(new Date().toISOString().slice(11, 19) + "  " + JSON.stringify(ev));
    handleServerEvent(ev);
  };
  state.ws.onclose = () => {
    state.ws = null;
    if (!state.done && document.body.classList.contains("state-listening")) {
      void finishNoMatch("SESSION ENDED");
    }
  };

  const mod = await import("/wasm/sivana_wasm.js");
  await mod.default();

  state.stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
  });
  state.ctx = new AudioContext({ sampleRate: FINGERPRINT_SAMPLE_RATE });
  state.resampler = new StreamingLinearResampler(
    state.ctx.sampleRate,
    FINGERPRINT_SAMPLE_RATE,
  );
  state.engine = new mod.WasmFingerprinter(FINGERPRINT_SAMPLE_RATE);
  note("engine: " + state.engine.version());
  note("audio: " + state.ctx.sampleRate + " Hz -> " + FINGERPRINT_SAMPLE_RATE + " Hz");

  const src = state.ctx.createMediaStreamSource(state.stream);
  const analyser = state.ctx.createAnalyser();
  analyser.fftSize = 512;
  analyser.smoothingTimeConstant = 0.78;
  src.connect(analyser);
  state.analyser = analyser;
  freqData = new Uint8Array(analyser.frequencyBinCount);
  document.getElementById("fact-signal").textContent = "GOOD";

  await state.ctx.audioWorklet.addModule("/wasm/pcm-worklet.js");
  const node = new AudioWorkletNode(state.ctx, "sivana-pcm-capture");
  node.port.onmessage = (e) => {
    const pcm = state.resampler.process(e.data);
    if (pcm.length === 0) return;
    const batch = state.engine.process(pcm);
    if (batch && batch.length > 16 && state.ws && state.ws.readyState === 1) {
      const n = new DataView(batch.buffer, batch.byteOffset + 12, 4).getUint32(0, true);
      state.landmarks += n;
      for (let i = 0; i < n; i++) {
        state.anchors.push({ t: performance.now(), a: Math.random() * Math.PI * 2 });
      }
      if (state.anchors.length > 240) state.anchors.splice(0, state.anchors.length - 240);
      state.ws.send(batch);
    }
  };
  src.connect(node);
  // The worklet only taps audio; give it a path to destination (muted)
  // or Chrome will never pull its process() callback and no batches flow.
  const mute = state.ctx.createGain();
  mute.gain.value = 0;
  node.connect(mute);
  mute.connect(state.ctx.destination);
  state.startedAt = performance.now();
  state.deadlineTimer = window.setTimeout(() => {
    if (!state.done && document.body.classList.contains("state-listening")) {
      void finishNoMatch();
    }
  }, CLIENT_LISTENING_LIMIT_MS);
}

async function finishNoMatch(status = "NO MATCH") {
  if (state.done) return;
  state.done = true;
  document.getElementById("fact-status").textContent = status;
  await stopListening();
  show("no-match");
}

function handleServerEvent(ev) {
  if (state.done) return;
  if (ev.event === "listening") {
    document.getElementById("fact-status").textContent = "BUILDING A MATCH";
  } else if (ev.event === "candidate") {
    document.getElementById("fact-status").textContent = "CANDIDATE";
  } else if (ev.event === "matched") {
    state.done = true;
    stopListening();
    renderResult(ev);
  } else if (ev.event === "no_match") {
    void finishNoMatch();
  }
}

function renderResult(ev) {
  const title = ev.title || ("Recording " + ev.recording_id);
  const artist = (ev.artist || "Unknown artist").replace(/\s+-\s+Topic$/i, "");
  const titleEl = document.getElementById("result-title");
  titleEl.textContent = title;
  titleEl.classList.toggle("result-title--long", title.length > 18);
  titleEl.classList.toggle("result-title--very-long", title.length > 30);
  document.getElementById("result-artist").textContent = artist;
  document.getElementById("result-record-number").textContent =
    String(Number(ev.recording_id) + 1).padStart(3, "0");

  const artwork = document.getElementById("result-artwork");
  const cover = document.getElementById("result-cover");
  artwork.classList.remove("has-artwork");
  cover.removeAttribute("src");
  cover.alt = "";
  if (ev.artwork_url) {
    cover.onload = () => artwork.classList.add("has-artwork");
    cover.onerror = () => artwork.classList.remove("has-artwork");
    cover.alt = "Cover artwork for " + title;
    cover.src = ev.artwork_url;
  }
  const hopSec = 1024 / 22050;
  const secs = ev.offset_frames * hopSec;
  document.getElementById("result-offset").textContent =
    Math.floor(secs / 60) + ":" + String(Math.floor(secs % 60)).padStart(2, "0");
  document.getElementById("result-confidence").textContent =
    ev.inliers + " inliers \u00B7 " + Math.round(ev.concentration * 100) + "%";
  document.getElementById("result-latency").textContent = Number(ev.capture_seconds).toFixed(2) + " s";
  document.getElementById("fact-status").textContent = "MATCHED";
  show("matched");
  addToArchive(ev);
}

function addToArchive(ev) {
  const key = "sivana-archive";
  const list = JSON.parse(localStorage.getItem(key) || "[]");
  list.unshift({
    title: ev.title || ("Recording " + ev.recording_id),
    artist: ev.artist || "Unknown artist",
    at: new Date().toISOString(),
  });
  localStorage.setItem(key, JSON.stringify(list.slice(0, 50)));
  renderArchive();
}

function renderArchive() {
  const list = JSON.parse(localStorage.getItem("sivana-archive") || "[]");
  const ol = document.getElementById("archive-list");
  const empty = document.getElementById("archive-empty");
  ol.innerHTML = "";
  empty.hidden = list.length > 0;
  list.forEach((item, i) => {
    const li = document.createElement("li");
    const idx = document.createElement("span");
    idx.className = "idx";
    idx.textContent = String(i + 1).padStart(2, "0");
    const body = document.createElement("div");
    body.textContent = item.title + " \u2014 " + item.artist;
    const time = document.createElement("time");
    time.textContent = new Date(item.at).toLocaleString();
    body.appendChild(time);
    li.append(idx, body);
    ol.appendChild(li);
  });
}

async function stopListening() {
  if (state.deadlineTimer) {
    window.clearTimeout(state.deadlineTimer);
    state.deadlineTimer = null;
  }
  if (state.ws) { try { state.ws.close(); } catch (e) {} state.ws = null; }
  if (state.stream) {
    for (const t of state.stream.getTracks()) t.stop();
    state.stream = null;
  }
  if (state.ctx) { try { await state.ctx.close(); } catch (e) {} state.ctx = null; }
  state.resampler = null;
  state.analyser = null;
  state.startedAt = 0;
}

function tick() {
  if (!state.done && state.startedAt && document.body.classList.contains("state-listening")) {
    const secs = (performance.now() - state.startedAt) / 1000;
    const wholeSeconds = Math.floor(secs);
    document.getElementById("fact-captured").textContent =
      String(Math.floor(wholeSeconds / 60)).padStart(2, "0") +
      ":" +
      String(wholeSeconds % 60).padStart(2, "0");
    document.getElementById("fact-landmarks").textContent = state.landmarks;
    if (secs > 2.5 && state.landmarks === 0) {
      document.getElementById("fact-signal").textContent = "NO INPUT";
      document.getElementById("fact-status").textContent = "NO MIC SIGNAL - CHECK INPUT DEVICE";
    } else if (secs > 1 && state.landmarks > 0) {
      document.getElementById("fact-signal").textContent = "GOOD";
      if (document.getElementById("fact-status").textContent.startsWith("NO MIC")) {
        document.getElementById("fact-status").textContent = "BUILDING A MATCH";
      }
    }
  }
  requestAnimationFrame(tick);
}

document.getElementById("listen-btn").addEventListener("click", () =>
  startListening().catch(async (e) => {
    state.done = true;
    await stopListening();
    note("error: " + e);
    const permissionDenied =
      e?.name === "NotAllowedError" ||
      String(e?.message || e).toLowerCase().includes("permission");
    document.getElementById("fact-status").textContent =
      permissionDenied ? "MIC PERMISSION NEEDED" : "RECOGNITION SERVICE OFFLINE";
    show("idle");
  })
);
document.getElementById("stop-btn").addEventListener("click", () => {
  state.done = true; stopListening(); show("idle");
  document.getElementById("fact-status").textContent = "IDLE";
});
document.getElementById("again-btn").addEventListener("click", () => {
  show("idle"); document.getElementById("fact-status").textContent = "IDLE";
});
document.getElementById("retry-btn").addEventListener("click", () => {
  show("idle"); document.getElementById("fact-status").textContent = "IDLE";
});

health();
renderArchive();
requestAnimationFrame(tick);
