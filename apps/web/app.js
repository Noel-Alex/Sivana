// Sivana web client: capture -> local fingerprinting -> streaming match.
// Raw audio never leaves the page; only SFP1 fingerprint batches are sent.

const $ = (id) => document.getElementById(id);
const state = {
  ws: null,
  session: null,
  engine: null,
  landmarks: 0,
  startedAt: 0,
  stream: null,
  ctx: null,
  anchors: [],
  done: false,
};

function show(stage) {
  for (const id of ["stage-hero", "stage-result", "stage-nomatch"]) {
    $(id).classList.add("hidden");
  }
  $(stage).classList.remove("hidden");
  if (stage === "stage-hero") $("listening-panel").classList.remove("hidden");
}

function colophon(line) {
  const pre = $("colo-body");
  pre.textContent += line + "\n";
}

async function health() {
  try {
    const r = await fetch("/v1/health");
    const j = await r.json();
    $("catalog-version").textContent = "v" + j.catalog_version;
  } catch {
    $("catalog-version").textContent = "offline";
  }
}

async function startListening() {
  $("listening-panel").classList.remove("hidden");
  $("listen-btn").classList.add("hidden");
  $("colo-body").textContent = "";
  state.landmarks = 0;
  state.anchors = [];
  state.done = false;

  // 1. Session
  const r = await fetch("/v1/sessions", { method: "POST" });
  const { session_id } = await r.json();
  state.session = session_id;

  // 2. WebSocket
  const wsUrl = (location.protocol === "https:" ? "wss://" : "ws://") + location.host + "/v1/identify/" + session_id;
  state.ws = new WebSocket(wsUrl);
  state.ws.binaryType = "arraybuffer";
  state.ws.onmessage = (e) => {
    const ev = JSON.parse(e.data);
    colophon(new Date().toISOString().slice(11, 19) + "  " + JSON.stringify(ev));
    handleServerEvent(ev);
  };

  // 3. Engine (local fingerprinting)
  try {
    const mod = await import("/wasm/sivana_wasm.js");
    await mod.default();
    state.engine = new mod.WasmFingerprinter(22050); // AudioContext will be created at this rate
    colophon("engine: " + state.engine.version());
  } catch (err) {
    colophon("engine unavailable: " + err);
    $("fact-status").textContent = "ENGINE UNAVAILABLE";
    return;
  }

  // 4. Microphone -> worklet -> engine -> WS
  state.stream = await navigator.mediaDevices.getUserMedia({
    audio: { echoCancellation: false, noiseSuppression: false, autoGainControl: false },
  });
  state.ctx = new AudioContext({ sampleRate: 22050 });
  await state.ctx.audioWorklet.addModule("/wasm/pcm-worklet.js");
  const node = new AudioWorkletNode(state.ctx, "sivana-pcm-capture");
  node.port.onmessage = (e) => {
    const batch = state.engine.process(e.data);
    if (batch && batch.length > 16 && state.ws && state.ws.readyState === 1) {
      state.landmarks += new DataView(batch.buffer, batch.byteOffset + 12, 4).getUint32(0, true);
      drawAnchors(batch);
      state.ws.send(batch);
    }
  };
  state.ctx.createMediaStreamSource(state.stream).connect(node);
  state.startedAt = performance.now();
  $("fact-signal").textContent = "GOOD";
}

function handleServerEvent(ev) {
  if (state.done) return;
  if (ev.event === "listening") {
    $("fact-status").textContent = "BUILDING A MATCH";
  } else if (ev.event === "candidate") {
    $("fact-status").textContent = "CANDIDATE — KEEP GOING";
  } else if (ev.event === "matched") {
    state.done = true;
    stopListening();
    renderResult(ev);
  } else if (ev.event === "no_match") {
    state.done = true;
    stopListening();
    show("stage-nomatch");
  }
}

function renderResult(ev) {
  $("result-title").textContent = ev.title || ("Recording " + ev.recording_id);
  $("result-artist").textContent = ev.artist || "Unknown artist";
  const hopSec = 1024 / 22050; // V2 geometry
  const mm = Math.floor((ev.offset_frames * hopSec) / 60);
  const ss = String(Math.floor((ev.offset_frames * hopSec) % 60)).padStart(2, "0");
  $("result-offset").textContent = mm + ":" + ss;
  $("result-confidence").textContent =
    ev.inliers + " inliers · " + Math.round(ev.concentration * 100) + "%";
  $("result-latency").textContent = Number(ev.capture_seconds).toFixed(2) + " s";
  show("stage-result");
  addToArchive(ev);
}

function addToArchive(ev) {
  const key = "sivana-archive";
  const list = JSON.parse(localStorage.getItem(key) || "[]");
  list.unshift({
    title: ev.title || "Recording " + ev.recording_id,
    artist: ev.artist || "Unknown artist",
    at: new Date().toISOString(),
  });
  localStorage.setItem(key, JSON.stringify(list.slice(0, 50)));
  renderArchive();
}

function renderArchive() {
  const list = JSON.parse(localStorage.getItem("sivana-archive") || "[]");
  const ol = $("archive-list");
  ol.innerHTML = "";
  list.forEach((item, i) => {
    const li = document.createElement("li");
    const idx = document.createElement("span");
    idx.className = "idx";
    idx.textContent = String(i + 1).padStart(2, "0");
    const name = document.createElement("span");
    name.textContent = item.title + " — " + item.artist;
    const time = document.createElement("time");
    time.textContent = new Date(item.at).toLocaleString();
    li.append(idx, name, time);
    ol.appendChild(li);
  });
  if (list.length) $("archive").classList.remove("hidden");
}

function drawAnchors(batch) {
  const n = new DataView(batch.buffer, batch.byteOffset + 12, 4).getUint32(0, true);
  const dv = new DataView(batch.buffer, batch.byteOffset);
  const canvas = $("constellation");
  const ctx2d = canvas.getContext("2d");
  const W = canvas.width, H = canvas.height;
  for (let i = 0; i < n; i++) {
    const o = 16 + i * 8;
    const hash = dv.getUint32(o, true);
    const t = dv.getUint32(o + 4, true);
    // Decorative-but-derived scatter: y from hash bits, x from anchor time.
    state.anchors.push({ x: (t * 3) % W, y: (hash % 1000) / 1000 * H });
  }
  ctx2d.clearRect(0, 0, W, H);
  ctx2d.fillStyle = "#D64518";
  for (const a of state.anchors.slice(-400)) {
    ctx2d.fillRect(a.x, a.y, 2, 2);
  }
}

async function stopListening() {
  if (state.ws) { try { state.ws.close(); } catch {} state.ws = null; }
  if (state.stream) {
    for (const t of state.stream.getTracks()) t.stop();
    state.stream = null;
  }
  if (state.ctx) { try { await state.ctx.close(); } catch {} state.ctx = null; }
  $("listen-btn").classList.remove("hidden");
}

function tick() {
  if (!state.done && state.startedAt) {
    $("fact-captured").textContent = ((performance.now() - state.startedAt) / 1000).toFixed(2) + " s";
    $("fact-landmarks").textContent = state.landmarks;
  }
  requestAnimationFrame(tick);
}

$("listen-btn").addEventListener("click", () => startListening().catch((e) => { colophon("error: " + e); $("fact-status").textContent = "MIC BLOCKED"; }));
$("stop-btn").addEventListener("click", () => { state.done = true; stopListening(); show("stage-hero"); });
$("again-btn").addEventListener("click", () => { show("stage-hero"); state.done = true; stopListening(); });
$("retry-btn").addEventListener("click", () => { show("stage-hero"); });
$("diag-toggle").addEventListener("change", (e) => {
  $("colophon").classList.toggle("hidden", !e.target.checked);
});

health();
renderArchive();
requestAnimationFrame(tick);
