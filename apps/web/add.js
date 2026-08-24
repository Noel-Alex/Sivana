const $ = (id) => document.getElementById(id);

const state = {
  file: null,
  metadata: null,
  busy: false,
  lookupTimer: null,
  lookupController: null,
  progressTimer: null,
};

function humanBytes(bytes) {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function setStatus(message, kind = "") {
  const status = $("ingest-status");
  status.textContent = message;
  status.dataset.kind = kind;
}

function updateReadiness() {
  const ready = Boolean(state.file && state.metadata && !state.busy);
  $("create-fingerprint").disabled = !ready;
  if (!state.busy && !ready) {
    setStatus(
      state.file
        ? "Add a valid YouTube Music reference to continue."
        : state.metadata
          ? "Drop the matching audio file to continue."
          : "Add a reference and an audio file to continue.",
    );
  } else if (!state.busy) {
    setStatus("Metadata and audio are ready to index.", "ready");
  }
}

function clearMetadata(message = "") {
  state.metadata = null;
  $("metadata-preview").classList.add("hidden");
  $("link-status").textContent = message;
  $("source-url").removeAttribute("aria-invalid");
  updateReadiness();
}

function renderMetadata(metadata) {
  state.metadata = metadata;
  $("metadata-title").textContent = metadata.title;
  $("metadata-artist").textContent = metadata.artist;
  $("metadata-artwork").src = metadata.artwork_url;
  $("metadata-artwork").alt = `Artwork for ${metadata.title}`;
  $("metadata-source").href = metadata.source_url;
  $("metadata-preview").classList.remove("hidden");
  $("link-status").textContent = "Metadata found.";
  $("source-url").removeAttribute("aria-invalid");
  updateReadiness();
}

async function lookupMetadata() {
  const url = $("source-url").value.trim();
  if (!url) {
    clearMetadata();
    return;
  }
  state.lookupController?.abort();
  const controller = new AbortController();
  state.lookupController = controller;
  state.metadata = null;
  $("metadata-preview").classList.add("hidden");
  $("link-status").textContent = "Reading the YouTube Music reference…";
  updateReadiness();
  try {
    const response = await fetch(`/v1/metadata/youtube?${new URLSearchParams({ url })}`, {
      signal: controller.signal,
    });
    const payload = await response.json();
    if (!response.ok) throw new Error(payload.error || "Metadata lookup failed.");
    if (state.lookupController !== controller) return;
    renderMetadata(payload);
  } catch (error) {
    if (error.name === "AbortError") return;
    state.metadata = null;
    $("source-url").setAttribute("aria-invalid", "true");
    $("link-status").textContent = error.message;
    updateReadiness();
  }
}

function scheduleMetadataLookup() {
  window.clearTimeout(state.lookupTimer);
  state.lookupTimer = window.setTimeout(lookupMetadata, 480);
}

function setFile(file) {
  if (!file) return;
  state.file = file;
  const extension = file.name.includes(".") ? file.name.split(".").pop() : "audio";
  $("file-kind").textContent = extension.toUpperCase().slice(0, 6);
  $("file-name").textContent = file.name;
  $("file-size").textContent = `${humanBytes(file.size)} · ready for local decoding`;
  $("file-ticket").classList.remove("hidden");
  $("drop-zone").classList.add("has-file");
  updateReadiness();
}

function removeFile() {
  state.file = null;
  $("audio-file").value = "";
  $("file-ticket").classList.add("hidden");
  $("drop-zone").classList.remove("has-file");
  updateReadiness();
}

function startProgressCopy() {
  const stages = [
    "Uploading the local audio…",
    "Decoding to a mono signal…",
    "Mapping spectral landmarks…",
    "Writing the catalog segment…",
  ];
  let index = 0;
  setStatus(stages[index], "working");
  state.progressTimer = window.setInterval(() => {
    index = Math.min(index + 1, stages.length - 1);
    setStatus(stages[index], "working");
  }, 1700);
}

function stopProgressCopy() {
  window.clearInterval(state.progressTimer);
  state.progressTimer = null;
}

async function createFingerprint(event) {
  event.preventDefault();
  if (!state.file || !state.metadata || state.busy) {
    updateReadiness();
    return;
  }
  state.busy = true;
  $("create-fingerprint").disabled = true;
  document.body.classList.add("is-ingesting");
  startProgressCopy();

  const form = new FormData();
  form.append("source_url", state.metadata.source_url);
  form.append("audio", state.file, state.file.name);
  try {
    const response = await fetch("/v1/recordings", { method: "POST", body: form });
    const payload = await response.json();
    if (!response.ok) throw new Error(payload.error || "Fingerprint creation failed.");
    $("catalog-version").textContent = `V${payload.catalog_version}`;
    setStatus(
      payload.duplicate
        ? `Already indexed as recording ${payload.recording_id}. Its metadata is now current.`
        : `Indexed as recording ${payload.recording_id}. It is ready for recognition.`,
      "success",
    );
    document.body.classList.add("ingest-complete");
    await loadCatalog();
  } catch (error) {
    setStatus(error.message, "error");
    document.body.classList.remove("ingest-complete");
  } finally {
    stopProgressCopy();
    state.busy = false;
    document.body.classList.remove("is-ingesting");
    $("create-fingerprint").disabled = !(state.file && state.metadata);
  }
}

async function loadCatalog() {
  try {
    const response = await fetch("/v1/recordings");
    if (!response.ok) throw new Error();
    const payload = await response.json();
    $("catalog-version").textContent = `V${payload.catalog_version}`;
    const list = $("catalog-list");
    list.innerHTML = "";
    $("ledger-count").textContent = `${payload.recordings.length} recording${payload.recordings.length === 1 ? "" : "s"}`;
    $("catalog-empty").hidden = payload.recordings.length > 0;
    payload.recordings.slice().reverse().forEach((recording) => {
      const item = document.createElement("li");
      const number = document.createElement("span");
      number.className = "catalog-list__number";
      number.textContent = String(recording.recording_id).padStart(3, "0");
      const artwork = document.createElement("img");
      artwork.alt = "";
      artwork.src = recording.artwork_url || "";
      artwork.hidden = !recording.artwork_url;
      const copy = document.createElement("div");
      const title = document.createElement("strong");
      title.textContent = recording.title;
      const artist = document.createElement("small");
      artist.textContent = recording.artist;
      copy.append(title, artist);
      const source = document.createElement(recording.source_url ? "a" : "span");
      source.className = "catalog-list__source";
      source.textContent = recording.source_url ? "YouTube Music ↗" : "Local metadata";
      if (recording.source_url) {
        source.href = recording.source_url;
        source.target = "_blank";
        source.rel = "noreferrer";
      }
      item.append(number, artwork, copy, source);
      list.appendChild(item);
    });
  } catch {
    $("ledger-count").textContent = "Catalog unavailable";
  }
}

$("source-url").addEventListener("input", scheduleMetadataLookup);
$("source-url").addEventListener("change", lookupMetadata);
$("audio-file").addEventListener("change", (event) => setFile(event.target.files[0]));
$("remove-file").addEventListener("click", removeFile);
$("ingest-form").addEventListener("submit", createFingerprint);

const dropZone = $("drop-zone");
for (const eventName of ["dragenter", "dragover"]) {
  dropZone.addEventListener(eventName, (event) => {
    event.preventDefault();
    dropZone.classList.add("is-dragging");
  });
}
for (const eventName of ["dragleave", "drop"]) {
  dropZone.addEventListener(eventName, (event) => {
    event.preventDefault();
    dropZone.classList.remove("is-dragging");
  });
}
dropZone.addEventListener("drop", (event) => setFile(event.dataTransfer.files[0]));

$("metadata-artwork").addEventListener("error", () => {
  $("metadata-artwork").hidden = true;
});
$("metadata-artwork").addEventListener("load", () => {
  $("metadata-artwork").hidden = false;
});

updateReadiness();
loadCatalog();
