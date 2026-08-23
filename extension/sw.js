// Sivana service worker: owns the offscreen document lifecycle.
// The recognition engine never lives here — audio capture and
// fingerprinting happen in the offscreen document (PLAN §59).

const OFFSCREEN_URL = "offscreen.html";

async function hasOffscreen() {
  const contexts = await chrome.runtime.getContexts({
    contextTypes: ["OFFSCREEN_DOCUMENT"],
  });
  return contexts.length > 0;
}

async function ensureOffscreen() {
  if (await hasOffscreen()) return;
  await chrome.offscreen.createDocument({
    url: OFFSCREEN_URL,
    reasons: ["AUDIO_PLAYBACK"],
    justification:
      "Tab-audio capture for on-device audio fingerprinting (no audio leaves the machine; only compact fingerprints are sent to the user-configured Sivana server).",
  });
}

chrome.runtime.onMessage.addListener((msg, sender, sendResponse) => {
  if (msg?.type === "ensure-offscreen") {
    ensureOffscreen().then(() => sendResponse({ ok: true })).catch((e) => {
      sendResponse({ ok: false, error: String(e) });
    });
    return true; // async response
  }
  if (msg?.type === "close-offscreen") {
    hasOffscreen().then((h) => {
      if (h) chrome.offscreen.closeDocument().catch(() => {});
    });
    sendResponse({ ok: true });
  }
});
