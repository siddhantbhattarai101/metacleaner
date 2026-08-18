"use strict";

const dropZone = document.getElementById("drop-zone");
const fileInput = document.getElementById("file-input");
const fileListSection = document.getElementById("file-list-section");
const fileListEl = document.getElementById("file-list");
const cleanAllBtn = document.getElementById("clean-all-btn");

const optEnhance = document.getElementById("opt-enhance");
const optFingerprint = document.getElementById("opt-fingerprint");
const noiseLevelRow = document.getElementById("noise-level-row");
const optNoiseLevel = document.getElementById("opt-noise-level");
const optQuality = document.getElementById("opt-quality");
const optQualityVal = document.getElementById("opt-quality-val");
const optFormat = document.getElementById("opt-format");

// Plain-language noise-level choices, mapped to the underlying
// strength/fraction knobs so nobody has to understand those directly.
const NOISE_LEVELS = {
  light: { strength: 1, fraction: 0.15 },
  medium: { strength: 2, fraction: 0.25 },
  strong: { strength: 3, fraction: 0.4 },
};

optQuality.addEventListener("input", () => (optQualityVal.textContent = optQuality.value));
optFingerprint.addEventListener("change", () => {
  noiseLevelRow.hidden = !optFingerprint.checked;
});

/** @type {Map<string, {file: File, id: string, inspect: object|null, inspectError: string|null, cleanState: "idle"|"working"|"done"|"error", cleanError: string|null}>} */
const entries = new Map();
let nextId = 0;

function addFiles(fileList) {
  const files = Array.from(fileList);
  if (files.length === 0) return;
  for (const file of files) {
    const id = `f${nextId++}`;
    entries.set(id, {
      file,
      id,
      inspect: null,
      inspectError: null,
      cleanState: "idle",
      cleanError: null,
    });
  }
  fileListSection.hidden = false;
  render();
  for (const [id, entry] of entries) {
    if (entry.inspect === null && entry.inspectError === null) {
      runInspect(id);
    }
  }
}

async function runInspect(id) {
  const entry = entries.get(id);
  if (!entry) return;
  const form = new FormData();
  form.append("file", entry.file, entry.file.name);
  try {
    const res = await fetch("/api/inspect", { method: "POST", body: form });
    const data = await res.json();
    if (data.ok) {
      entry.inspect = data;
    } else {
      entry.inspectError = data.error || "inspect failed";
    }
  } catch (e) {
    entry.inspectError = String(e);
  }
  render();
}

async function runClean(id) {
  const entry = entries.get(id);
  if (!entry) return;
  entry.cleanState = "working";
  entry.cleanError = null;
  render();

  const noise = NOISE_LEVELS[optNoiseLevel.value] || NOISE_LEVELS.medium;

  const form = new FormData();
  form.append("file", entry.file, entry.file.name);
  form.append("enhance", optEnhance.checked ? "true" : "false");
  form.append("reset_fingerprint", optFingerprint.checked ? "true" : "false");
  form.append("fingerprint_strength", String(noise.strength));
  form.append("fingerprint_fraction", String(noise.fraction));
  form.append("jpeg_quality", optQuality.value);
  if (optFormat.value) form.append("format", optFormat.value);

  try {
    const res = await fetch("/api/clean", { method: "POST", body: form });
    const data = await res.json();
    if (!data.ok) {
      entry.cleanState = "error";
      entry.cleanError = data.error || "clean failed";
      render();
      return;
    }
    downloadBase64(data.data_base64, data.filename, data.mime);
    entry.cleanState = "done";
    entry.cleanReport = data;
  } catch (e) {
    entry.cleanState = "error";
    entry.cleanError = String(e);
  }
  render();
}

function downloadBase64(base64, filename, mime) {
  const bytes = atob(base64);
  const buf = new Uint8Array(bytes.length);
  for (let i = 0; i < bytes.length; i++) buf[i] = bytes.charCodeAt(i);
  const blob = new Blob([buf], { type: mime || "application/octet-stream" });
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  setTimeout(() => URL.revokeObjectURL(url), 10000);
}

function formatBytes(n) {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  return `${(n / (1024 * 1024)).toFixed(2)} MB`;
}

function render() {
  fileListEl.innerHTML = "";
  for (const entry of entries.values()) {
    fileListEl.appendChild(renderCard(entry));
  }
  const anyWorking = Array.from(entries.values()).some((e) => e.cleanState === "working");
  cleanAllBtn.disabled = entries.size === 0 || anyWorking;
}

function renderCard(entry) {
  const card = document.createElement("div");
  card.className = "file-card";

  const header = document.createElement("div");
  header.className = "file-card-header";

  const name = document.createElement("span");
  name.className = "file-name";
  name.textContent = entry.file.name;
  header.appendChild(name);

  const meta = document.createElement("span");
  meta.className = "file-meta";
  if (entry.inspect) {
    meta.textContent = `${entry.inspect.format.toUpperCase()} ${entry.inspect.width}x${entry.inspect.height} · ${formatBytes(entry.inspect.bytes)}`;
  } else if (entry.inspectError) {
    meta.textContent = "unreadable";
  } else {
    meta.textContent = "inspecting…";
  }
  header.appendChild(meta);

  card.appendChild(header);

  if (entry.inspect) {
    const badge = document.createElement("span");
    if (entry.inspect.clean) {
      badge.className = "badge badge-ok";
      badge.textContent = "no metadata found";
    } else {
      badge.className = "badge badge-warn";
      badge.textContent = `${entry.inspect.findings.length} finding${entry.inspect.findings.length === 1 ? "" : "s"}`;
    }
    card.appendChild(badge);

    if (entry.inspect.findings.length > 0) {
      const findings = document.createElement("div");
      findings.className = "findings";
      for (const f of entry.inspect.findings) {
        const row = document.createElement("div");
        row.className = "finding-row";
        const cat = document.createElement("span");
        cat.className = "cat";
        cat.textContent = `[${f.category}]`;
        const label = document.createElement("span");
        label.textContent = `${f.label} (${formatBytes(f.size_bytes)})`;
        row.appendChild(cat);
        row.appendChild(label);
        findings.appendChild(row);
      }
      card.appendChild(findings);
    }
  } else if (entry.inspectError) {
    const badge = document.createElement("span");
    badge.className = "badge badge-err";
    badge.textContent = "error";
    card.appendChild(badge);
    const err = document.createElement("div");
    err.className = "status-line err";
    err.textContent = entry.inspectError;
    card.appendChild(err);
  }

  if (entry.cleanState === "working") {
    const s = document.createElement("div");
    s.className = "status-line";
    s.textContent = "cleaning…";
    card.appendChild(s);
  } else if (entry.cleanState === "done") {
    const s = document.createElement("div");
    s.className = "status-line";
    s.textContent = "cleaned and downloaded";
    card.appendChild(s);
  } else if (entry.cleanState === "error") {
    const s = document.createElement("div");
    s.className = "status-line err";
    s.textContent = entry.cleanError;
    card.appendChild(s);
  }

  return card;
}

dropZone.addEventListener("dragover", (e) => {
  e.preventDefault();
  dropZone.classList.add("drag-over");
});
dropZone.addEventListener("dragleave", () => dropZone.classList.remove("drag-over"));
dropZone.addEventListener("drop", (e) => {
  e.preventDefault();
  dropZone.classList.remove("drag-over");
  if (e.dataTransfer && e.dataTransfer.files) addFiles(e.dataTransfer.files);
});

fileInput.addEventListener("change", () => {
  addFiles(fileInput.files);
  fileInput.value = "";
});

cleanAllBtn.addEventListener("click", () => {
  for (const id of entries.keys()) runClean(id);
});
