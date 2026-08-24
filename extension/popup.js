// Sivana popup: gesture entry point for chrome.tabCapture.

const $ = (id) => document.getElementById(id);

let port = null;

function setStatus(t) {
  document.getElementById("status").textContent = t;
}

function showResult(ev) {
  const el = (id) => document.getElementById(id);
  el("result").style.display = "block";
  el("r-title").textContent = ev.title || ("Recording " + ev.recording_id);
  el("r-artist").textContent = ev.artist || "Unknown artist";
  const hopSec = 1024 / 22050;
  const secs = ev.offset_frames * hopSec;
  el("r-offset").textContent =
    Math.floor(secs / 60) + ":" + String(Math.floor(secs % 60)).padStart(2, "0");
  el("r-latency").textContent = Number(ev.capture_seconds).toFixed(1) + "s";
}

async function start() {
  setStatus("REQUESTING TAB AUDIO");
  await chrome.runtime.sendMessage({ type: "ensure-offscreen" });

  // Capture must happen in a user-gesture context.
  const stream = await chrome.tabCapture.getMediaStream({ target: "tab" });
  setStatus("CAPTURING");

  const server =
    (await chrome.storage.local.get(["server"])).server ||
    document.getElementById("server").value;

  // Hand the live stream to the offscreen document, which runs the engine.
  port = chrome.runtime.connect({ name: "capture" });
  port.onMessage.addListener((msg) => {
    if (msg.type === "ev") {
      if (msg.ev.event === "matched") showResult(msg.ev);
      else if (msg.ev.event === "no_match") setStatus("NOT IN CATALOG");
      else if (msg.ev.event === "error") setStatus("ERROR: " + msg.ev.detail);
      else setStatus(msg.ev.event.toUpperCase());
    }
    if (msg.type === "done") setStatus("DONE");
  });
  port.onDisconnect.addListener(() => {
    if (chrome.runtime.lastError) setStatus("CHANNEL LOST");
  });
  port.postMessage({ type: "start", stream, server });
}

document.getElementById("listen").addEventListener("click", () => {
  start().catch((e) => setStatus("FAILED: " + e.message));
});

chrome.storage.session.get(["lastResult"], (g) => {
  if (g.lastResult) showResult(g.lastResult);
});
