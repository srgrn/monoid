import { mergeQueue, summarizeQueue } from "./queue.js";
import { setupUpdater } from "./updater.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
const appWindow = window.__TAURI__.window?.getCurrentWindow?.();
const LogicalSize = window.__TAURI__.dpi?.LogicalSize;

let queue = [];
let running = false;
let animTimer = null;
let outputDirInput;
let filenameTemplateInput;
let overwritePolicyInput;
let progressState = {
  total: 0,
  completed: 0,
  succeeded: 0,
  failed: 0,
  skipped: 0,
  cancelled: 0,
  currentFile: null,
  currentFileProgress: 0,
  overallProgress: 0,
  message: "",
};

function byId(id) {
  return document.getElementById(id);
}

function waitForNextFrame() {
  return new Promise((resolve) => {
    requestAnimationFrame(() => resolve());
  });
}

async function autofitWindowToContent() {
  if (!appWindow || !LogicalSize) {
    return;
  }

  await waitForNextFrame();
  await waitForNextFrame();

  const shell = document.querySelector(".shell");
  if (!shell) {
    return;
  }

  const shellStyles = window.getComputedStyle(shell);
  const bodyStyles = window.getComputedStyle(document.body);
  const horizontalPadding =
    parseFloat(shellStyles.marginLeft || "0") +
    parseFloat(shellStyles.marginRight || "0") +
    parseFloat(bodyStyles.paddingLeft || "0") +
    parseFloat(bodyStyles.paddingRight || "0");
  const verticalPadding =
    parseFloat(shellStyles.paddingTop || "0") +
    parseFloat(shellStyles.paddingBottom || "0") +
    parseFloat(bodyStyles.paddingTop || "0") +
    parseFloat(bodyStyles.paddingBottom || "0");

  const targetWidth = Math.max(1080, Math.ceil(shell.scrollWidth + horizontalPadding + 40));
  const targetHeight = Math.max(760, Math.ceil(shell.scrollHeight + verticalPadding + 40));

  try {
    await appWindow.setSize(new LogicalSize(targetWidth, targetHeight));
    await appWindow.center();
  } catch (error) {
    console.warn("Unable to autofit Monoid window", error);
  }
}

/* ── UI helpers ── */

function setStatus(message, tone = "neutral") {
  const statusEl = byId("batch-status");
  statusEl.textContent = message;
  statusEl.dataset.tone = tone;
}

function setRunningState(nextRunning) {
  running = nextRunning;
  document.body.dataset.running = nextRunning;
  const hasFiles = queue.length > 0;

  byId("add-files").disabled = nextRunning;
  byId("add-folder").disabled = nextRunning;
  byId("clear-queue").disabled = nextRunning || !hasFiles;
  byId("start-batch").disabled = nextRunning || !hasFiles;
  byId("cancel-batch").disabled = !nextRunning;
  byId("skip-existing").disabled = nextRunning;
  byId("stop-on-error").disabled = nextRunning;
  byId("output-dir").disabled = nextRunning;
  byId("select-output-dir").disabled = nextRunning;
  byId("filename-template").disabled = nextRunning;
  byId("overwrite-policy").disabled = nextRunning;
}

function updateSummaryCards() {
  const queueSummary = summarizeQueue(queue);

  // Metric chips in action bar
  byId("queue-done").textContent = String(queueSummary.done);
  byId("queue-failed").textContent = String(queueSummary.failed);
  byId("queue-ready").textContent = String(queueSummary.ready);

  // Hidden compat elements
  byId("queue-total").textContent = String(queueSummary.total);

  // Show metrics when there's something to show
  const metricsEl = byId("action-metrics");
  metricsEl.hidden = queueSummary.total === 0;

  // Progress footer
  const progressFooter = byId("progress-footer");
  const showProgress = running || progressState.completed > 0;
  progressFooter.hidden = !showProgress;

  if (showProgress) {
    byId("progress-fill").style.width = `${progressState.overallProgress}%`;
    byId("batch-progress").value = progressState.overallProgress;
    byId("batch-progress-label").textContent = `${progressState.overallProgress.toFixed(1)}%`;

    byId("batch-detail").textContent = progressState.currentFile
      ? `${progressState.message || "Processing"}: ${progressState.currentFile}`
      : progressState.message || "Idle";

    byId("batch-stats").textContent = `${progressState.completed}/${progressState.total || queueSummary.total}`;
  }
}

function updateQueueVisibility() {
  const hasFiles = queue.length > 0;
  byId("dropzone").hidden = hasFiles;
  byId("queue-container").hidden = !hasFiles;
}

function renderQueue(animate = false) {
  const queueEl = byId("queue-list");
  const queueContainer = byId("queue-container");
  const summary = summarizeQueue(queue);

  updateQueueVisibility();

  if (queue.length === 0) {
    queueEl.innerHTML = "";
  } else {
    queueEl.innerHTML = queue
      .map((entry, index) => {
        const filename = entry.path.split(/[\\\\/]/).pop();
        const dir = entry.path.slice(0, entry.path.length - filename.length - 1);
        const meta = entry.message || entry.outputPath || entry.status;
        return `
          <li class="queue-item" data-status="${entry.status}" style="--delay:${index * 25}ms">
            <span class="queue-item-indicator"></span>
            <div class="queue-item-body">
              <div class="queue-item-name">${filename}</div>
              <div class="queue-item-path">${dir}</div>
            </div>
            <span class="queue-item-meta">${meta}</span>
          </li>`;
      })
      .join("");
  }

  if (animate && queue.length > 0) {
    queueContainer.dataset.animate = '';
    clearTimeout(animTimer);
    animTimer = setTimeout(() => {
      delete queueContainer.dataset.animate;
    }, Math.min(queue.length * 25 + 600, 2000));
  }

  byId("queue-caption").textContent = `${summary.total} file${summary.total !== 1 ? "s" : ""}`;
  updateSummaryCards();
  setRunningState(running);
}

function resetQueueStatuses() {
  queue = queue.map((entry) => ({
    ...entry,
    status: "ready",
    message: "",
    outputPath: "",
  }));
}

function upsertQueuePaths(paths) {
  queue = mergeQueue(queue, paths);
  renderQueue(true);
}

function updateQueueItem(payload) {
  queue = queue.map((entry) =>
    entry.path === payload.filePath
      ? {
          ...entry,
          status: payload.status,
          message: payload.message || "",
          outputPath: payload.outputPath || "",
        }
      : entry,
  );
  renderQueue();
}

/* ── Actions ── */

async function addFiles() {
  const selected = await window.__TAURI__.dialog.open({
    multiple: true,
    filters: [
      {
        name: "Audio Files",
        extensions: ["wav", "mp3", "flac", "aac", "ogg", "m4a", "mp4", "aiff", "caf", "mkv"],
      },
    ],
  });

  if (!selected) {
    return;
  }

  upsertQueuePaths(Array.isArray(selected) ? selected : [selected]);
  setStatus("Files added.", "neutral");
}

async function addFolder() {
  const selected = await window.__TAURI__.dialog.open({
    directory: true,
    multiple: false,
  });

  if (!selected || Array.isArray(selected)) {
    return;
  }

  const files = await invoke("list_supported_audio_files", { folderPath: selected });
  upsertQueuePaths(files);
  setStatus(files.length ? `Added ${files.length} files.` : "No audio files found.", files.length ? "neutral" : "warning");
}

async function selectOutputDir() {
  const selected = await window.__TAURI__.dialog.open({
    directory: true,
    multiple: false,
    defaultPath: outputDirInput.value || undefined,
  });

  if (!selected || Array.isArray(selected)) {
    return;
  }

  outputDirInput.value = selected;
}

async function startBatch() {
  if (queue.length === 0 || running) {
    return;
  }

  resetQueueStatuses();
  progressState = {
    total: queue.length,
    completed: 0,
    succeeded: 0,
    failed: 0,
    skipped: 0,
    cancelled: 0,
    currentFile: null,
    currentFileProgress: 0,
    overallProgress: 0,
    message: "Starting batch conversion",
  };
  renderQueue();
  setRunningState(true);
  setStatus("Converting...");

  try {
    await invoke("start_batch_conversion", {
      filePaths: queue.map((entry) => entry.path),
      options: {
        skipExistingOutputs: byId("skip-existing").checked,
        stopOnError: byId("stop-on-error").checked,
        outputDir: outputDirInput.value.trim(),
        filenameTemplate: filenameTemplateInput.value.trim(),
        overwritePolicy: overwritePolicyInput.value,
      },
    });
  } catch (error) {
    setRunningState(false);
    setStatus(`Error: ${error}`, "error");
  }
}

async function cancelBatch() {
  await invoke("cancel_conversion");
  setStatus("Cancelling...", "warning");
}

/* ── Drag and drop ── */

async function setupDragAndDrop() {
  const dropzone = byId("dropzone");

  // Tauri v2 intercepts file drops at the webview level,
  // so we use the native onDragDropEvent API instead of browser drag events.
  const getCurrentWebview = window.__TAURI__.webview?.getCurrentWebview;
  if (!getCurrentWebview) return;

  await getCurrentWebview().onDragDropEvent((event) => {
    if (event.payload.type === "over") {
      dropzone.classList.add("drag-over");
    } else if (event.payload.type === "drop") {
      dropzone.classList.remove("drag-over");
      const paths = event.payload.paths || [];
      if (paths.length > 0) {
        upsertQueuePaths(paths);
        setStatus(`Added ${paths.length} file${paths.length !== 1 ? "s" : ""}.`);
      }
    } else {
      // cancelled
      dropzone.classList.remove("drag-over");
    }
  });
}

/* ── Settings toggle ── */

function setupSettingsToggle() {
  const btn = byId("toggle-settings");
  const panel = byId("settings-panel");

  btn.addEventListener("click", () => {
    const isHidden = panel.hidden;
    panel.hidden = !isHidden;
    btn.classList.toggle("active", isHidden);
  });
}

/* ── Init ── */

window.addEventListener("DOMContentLoaded", async () => {
  outputDirInput = byId("output-dir");
  filenameTemplateInput = byId("filename-template");
  overwritePolicyInput = byId("overwrite-policy");

  renderQueue();
  setupDragAndDrop();
  setupSettingsToggle();
  await autofitWindowToContent();
  setupUpdater();

  // Show app version in topbar
  const getVersion = window.__TAURI__.app?.getVersion;
  if (getVersion) {
    try {
      byId("app-version").textContent = `v${await getVersion()}`;
    } catch {}
  }

  await Promise.all([
    listen("batch-item", (event) => {
      updateQueueItem(event.payload);
    }),
    listen("batch-progress", (event) => {
      progressState = event.payload;
      updateSummaryCards();
    }),
    listen("batch-finished", (event) => {
      progressState = event.payload;
      setRunningState(false);
      updateSummaryCards();

      const tone = event.payload.failed > 0 ? "warning" : event.payload.cancelled > 0 ? "warning" : "success";
      setStatus(event.payload.message || "Batch finished.", tone);
    }),
  ]);

  byId("add-files").addEventListener("click", () => {
    addFiles().catch((error) => setStatus(`File picker failed: ${error}`, "error"));
  });
  byId("add-folder").addEventListener("click", () => {
    addFolder().catch((error) => setStatus(`Folder scan failed: ${error}`, "error"));
  });
  byId("select-output-dir").addEventListener("click", () => {
    selectOutputDir().catch((error) => setStatus(`Output directory selection failed: ${error}`, "error"));
  });
  byId("clear-queue").addEventListener("click", () => {
    queue = [];
    progressState = {
      total: 0,
      completed: 0,
      succeeded: 0,
      failed: 0,
      skipped: 0,
      cancelled: 0,
      currentFile: null,
      currentFileProgress: 0,
      overallProgress: 0,
      message: "Queue cleared",
    };
    renderQueue();
    setStatus("Ready");
  });
  byId("start-batch").addEventListener("click", () => {
    startBatch();
  });
  byId("cancel-batch").addEventListener("click", () => {
    cancelBatch().catch((error) => setStatus(`Cancel failed: ${error}`, "error"));
  });
});
