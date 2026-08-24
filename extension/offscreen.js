// Sivana offscreen document: tab-audio capture + local fingerprinting.
// Reuses the exact engine and PCM worklet from the website build - no
// separate recognition engine (PLAN §59).

const FINGERPRINT_SAMPLE_RATE = 22050;

let ctx = null;
let ws = null;
let engine = null;
let session = null;
let stream = null;
let port = null;
let resampler = null;
let finished = true;

// AudioContext's requested sample rate is only a preference on some devices.
// Keep the fingerprint geometry identical to ingestion even when the browser
// delivers tab PCM at the hardware rate (commonly 44.1 or 48 kHz).
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

// Events go out the same way the rest of the extension talks: the capture
// port (popup listens there) plus a runtime broadcast (service worker).
function emit(ev) {
  try { port?.postMessage({ type: "ev", ev }); } catch {}
  chrome.runtime.sendMessage({ type: "ev", ev }).catch(() => {});
}

async function startCapture(captureStream, serverUrl) {
  stream = captureStream;
  finished = false;

  // 1. Session
  const r = await fetch(serverUrl.replace(/^ws/, "http") + "/v1/sessions", {
    method: "POST",
  });
  session = (await r.json()).session_id;

  // 2. WebSocket
  ws = new WebSocket(serverUrl + "/v1/identify/" + session);
  ws.binaryType = "arraybuffer";
  ws.onmessage = (e) => {
    const ev = JSON.parse(e.data);
    emit(ev);
    if (ev.event === "matched" || ev.event === "no_match") {
      if (ev.event === "matched") chrome.storage.session.set({ lastResult: ev });
      finish();
    }
  };
  ws.onerror = () => {};
  ws.onclose = () => {
    ws = null;
    // Socket died mid-listen without a terminal event: tear down and tell
    // the popup instead of hanging forever.
    if (!finished) finish({ event: "error", detail: "recognition socket closed unexpectedly" });
  };

  // 3. Engine
  const mod = await import(chrome.runtime.getURL("wasm/sivana_wasm.js"));
  await mod.default();

  // 4. Audio graph: MediaStream -> worklet -> resampler -> engine -> WS
  ctx = new AudioContext({ sampleRate: FINGERPRINT_SAMPLE_RATE });
  resampler = new StreamingLinearResampler(ctx.sampleRate, FINGERPRINT_SAMPLE_RATE);
  engine = new mod.WasmFingerprinter(FINGERPRINT_SAMPLE_RATE);

  await ctx.audioWorklet.addModule(chrome.runtime.getURL("pcm-worklet.js"));
  const node = new AudioWorkletNode(ctx, "sivana-pcm-capture");
  node.port.onmessage = (e) => {
    if (!ws || ws.readyState !== 1) return;
    const pcm = resampler.process(e.data);
    if (pcm.length === 0) return;
    const batch = engine.process(pcm);
    if (batch.length > 16) ws.send(batch);
  };
  ctx.createMediaStreamSource(stream).connect(node);
  // The worklet only taps audio; give it a path to destination (muted)
  // or Chrome will never pull its process() callback and no batches flow.
  const mute = ctx.createGain();
  mute.gain.value = 0;
  node.connect(mute);
  mute.connect(ctx.destination);
}

function finish(extraEv) {
  if (finished) return;
  finished = true;
  cleanup();
  if (extraEv) emit(extraEv);
  try { port?.postMessage({ type: "done" }); } catch {}
  chrome.runtime.sendMessage({ type: "done" }).catch(() => {});
}

function cleanup() {
  if (ws) { try { ws.close(); } catch {} ws = null; }
  if (stream) { for (const t of stream.getTracks()) t.stop(); stream = null; }
  if (ctx) { try { ctx.close(); } catch {} ctx = null; }
  resampler = null;
  engine = null;
}

// Streams arrive over a runtime Port (MediaStreamTracks are port-
// transferable; they do not survive plain sendMessage).
chrome.runtime.onConnect.addListener((p) => {
  if (p.name !== "capture") return;
  port = p;
  p.onDisconnect.addListener(() => { if (port === p) port = null; });
  p.onMessage.addListener((msg) => {
    if (msg?.type === "start" && msg.stream) {
      startCapture(msg.stream, msg.server).catch((e) => {
        finished = true;
        emit({ event: "error", detail: String(e) });
        cleanup();
      });
    }
  });
});
