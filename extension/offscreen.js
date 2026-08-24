// Sivana offscreen document: tab-audio capture + local fingerprinting.
// Reuses the exact engine and PCM worklet from the website build - no
// separate recognition engine (PLAN §59).

let ctx = null;
let ws = null;
let engine = null;
let session = null;

async function startCapture(stream, serverUrl) {
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
    chrome.runtime.sendMessage({ type: "ev", ev }).catch(() => {});
    if (ev.event === "matched" || ev.event === "no_match") {
      cleanup();
      chrome.runtime.sendMessage({ type: "done" }).catch(() => {});
    }
    if (ev.event === "matched") {
      chrome.storage.session.set({ lastResult: ev });
    }
  };

  // 3. Engine
  const mod = await import(chrome.runtime.getURL("wasm/sivana_wasm.js"));
  await mod.default();
  engine = new mod.WasmFingerprinter(22050);

  // 4. Audio graph: MediaStream -> worklet -> engine -> WS
  ctx = new AudioContext({ sampleRate: 22050 });
  await ctx.audioWorklet.addModule(chrome.runtime.getURL("pcm-worklet.js"));
  const node = new AudioWorkletNode(ctx, "sivana-pcm-capture");
  node.port.onmessage = (e) => {
    if (!ws || ws.readyState !== 1) return;
    const batch = engine.process(e.data);
    if (batch.length > 16) ws.send(batch);
  };
  ctx.createMediaStreamSource(stream).connect(node);
}

function cleanup() {
  if (ws) { try { ws.close(); } catch {} ws = null; }
  if (ctx) { try { ctx.close(); } catch {} ctx = null; }
  engine = null;
}

// Streams arrive over a runtime Port (MediaStreamTracks are port-
// transferable; they do not survive plain sendMessage).
chrome.runtime.onConnect.addListener((port) => {
  if (port.name !== "capture") return;
  port.onMessage.addListener((msg) => {
    if (msg?.type === "start" && msg.stream) {
      startCapture(msg.stream, msg.server).catch((e) => {
        port.postMessage({ type: "ev", ev: { event: "error", detail: String(e) } });
      });
    }
  });
});
