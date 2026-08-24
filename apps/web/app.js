// Sivana client: capture -> local fingerprinting -> streaming match.
// Raw audio never leaves the page; only SFP1 fingerprint batches are sent.
// The visualizer reads the same audio via an AnalyserNode tap.

const $ = (id) => document.getElementById(id);
const state = {
  ws: null, session: null, engine: null, analyser: null,
  landmarks: 0, startedAt: 0, stream: null, ctx: null,
  anchors: [], done: false, level: 0,
};

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
  document.getElementById(btnId).addEventListener("click", () => {
    const d = document.getElementById(drawerId);
    d.classList.toggle("open");
    for (const other of ["drawer-archive", "drawer-notes"]) {
      if (other !== drawerId) document.getElementById(other).classList.remove("open");
    }
  });
}
bindDrawer("toggle-archive", "drawer-archive");
bindDrawer("toggle-notes", "drawer-notes");

const viz = document.getElementById("viz");
const vctx = viz.getContext("2d");
let freqData = null;

function resizeViz() {
  viz.width = window.innerWidth * devicePixelRatio;
  viz.height = window.innerHeight * devicePixelRatio;
}
window.addEventListener("resize", resizeViz);
resizeViz();

const BARS = 96;

function drawViz(t) {
  // Self-heal sizing: hidden panes report 0x0 until first paint.
  const wantW = Math.max(1, window.innerWidth * devicePixelRatio);
  const wantH = Math.max(1, window.innerHeight * devicePixelRatio);
  if (viz.width !== wantW || viz.height !== wantH) resizeViz();
  const W = viz.width, H = viz.height;
  const cx = W / 2, cy = H / 2;
  const base = Math.min(W, H) * 0.21;
  vctx.clearRect(0, 0, W, H);

  let levels = new Float32Array(BARS);
  if (state.analyser && freqData) {
    state.analyser.getByteFrequencyData(freqData);
    let sum = 0;
    for (let i = 0; i < BARS; i++) {
      const bin = Math.floor(Math.pow(i / BARS, 1.6) * freqData.length * 0.75);
      levels[i] = freqData[bin] / 255;
      sum += levels[i];
    }
    state.level = state.level * 0.85 + (sum / BARS) * 0.15;
  } else {
    const idle = 0.06 + 0.04 * Math.sin(t / 900);
    for (let i = 0; i < BARS; i++) {
      levels[i] = idle + 0.03 * Math.sin(t / 500 + i * 0.4);
    }
  }

  const rot = t / 24000;
  vctx.save();
  vctx.translate(cx, cy);
  vctx.rotate(rot);
  for (let i = 0; i < BARS; i++) {
    const a = (i / BARS) * Math.PI * 2;
    const len = base * (0.12 + levels[i] * 1.15);
    const x1 = Math.cos(a) * base;
    const y1 = Math.sin(a) * base;
    const x2 = Math.cos(a) * (base + len);
    const y2 = Math.sin(a) * (base + len);
    const hot = levels[i];
    vctx.strokeStyle =
      hot > 0.55 ? "#FF4A1C" : hot > 0.25 ? "rgba(255,74,28,0.75)" : "rgba(243,237,227,0.28)";
    vctx.lineWidth = Math.max(1.5, (W / 1400) * 2.2);
    vctx.beginPath();
    vctx.moveTo(x1, y1);
    vctx.lineTo(x2, y2);
    vctx.stroke();
  }
  vctx.restore();

  const pulse = 1 + state.level * 0.5;
  vctx.strokeStyle = "rgba(255,74,28,0.8)";
  vctx.lineWidth = Math.max(1, (W / 1400) * 1.4);
  vctx.beginPath();
  vctx.arc(cx, cy, base * 0.92 * pulse, 0, Math.PI * 2);
  vctx.stroke();

  vctx.strokeStyle = "rgba(243,237,227,0.12)";
  vctx.beginPath();
  vctx.arc(cx, cy, base * 0.8, 0, Math.PI * 2);
  vctx.stroke();

  vctx.fillStyle = "#FF4A1C";
  for (const blip of state.anchors) {
    const age = (t - blip.t) / 2600;
    if (age > 1) continue;
    const r = base * (1.05 + age * 0.9);
    const a = blip.a + t / 4000;
    vctx.globalAlpha = (1 - age) * 0.9;
    vctx.fillRect(cx + Math.cos(a) * r, cy + Math.sin(a) * r, 2.5, 2.5);
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

  const mod = await import("/wasm/sivana_wasm.js");
  await mod.default();
  state.engine = new mod.WasmFingerprinter(22050);
  note("engine: " + state.engine.version());

  state.stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
  });
  state.ctx = new AudioContext({ sampleRate: 22050 });

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
    const batch = state.engine.process(e.data);
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
    state.done = true;
    stopListening();
    document.getElementById("fact-status").textContent = "NO MATCH";
    show("no-match");
  }
}

function renderResult(ev) {
  document.getElementById("result-title").textContent = ev.title || ("Recording " + ev.recording_id);
  document.getElementById("result-artist").textContent = ev.artist || "Unknown artist";
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
  ol.innerHTML = "";
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
  if (state.ws) { try { state.ws.close(); } catch (e) {} state.ws = null; }
  if (state.stream) {
    for (const t of state.stream.getTracks()) t.stop();
    state.stream = null;
  }
  if (state.ctx) { try { await state.ctx.close(); } catch (e) {} state.ctx = null; }
  state.analyser = null;
}

function tick() {
  if (!state.done && state.startedAt && document.body.classList.contains("state-listening")) {
    const secs = (performance.now() - state.startedAt) / 1000;
    document.getElementById("fact-captured").textContent = secs.toFixed(2);
    document.getElementById("fact-landmarks").textContent = state.landmarks;
    if (secs > 2.5 && state.landmarks === 0) {
      document.getElementById("fact-signal").textContent = "NO INPUT";
      document.getElementById("fact-status").textContent = "NO MIC SIGNAL - CHECK INPUT DEVICE";
    } else if (secs > 1 && state.landmarks > 0) {
      document.getElementById("fact-signal").textContent = "GOOD";
    }
  }
  requestAnimationFrame(tick);
}

document.getElementById("listen-btn").addEventListener("click", () =>
  startListening().catch((e) => {
    note("error: " + e);
    document.getElementById("fact-status").textContent = "MIC BLOCKED";
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
