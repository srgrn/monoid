import { mergeQueue, summarizeQueue } from "./queue.js";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let queue = [];
let running = false;
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

function setStatus(message, tone = "neutral") {
  const statusEl = byId("batch-status");
  statusEl.textContent = message;
  statusEl.dataset.tone = tone;
}

function setRunningState(nextRunning) {
  running = nextRunning;
  byId("add-files").disabled = nextRunning;
  byId("add-folder").disabled = nextRunning;
  byId("clear-queue").disabled = nextRunning || queue.length === 0;
  byId("start-batch").disabled = nextRunning || queue.length === 0;
  byId("cancel-batch").disabled = !nextRunning;
  byId("skip-existing").disabled = nextRunning;
  byId("stop-on-error").disabled = nextRunning;
}

function updateSummaryCards() {
  const queueSummary = summarizeQueue(queue);
  byId("queue-total").textContent = String(queueSummary.total);
  byId("queue-ready").textContent = String(queueSummary.ready);
  byId("queue-done").textContent = String(queueSummary.done);
  byId("queue-failed").textContent = String(queueSummary.failed);

  byId("batch-progress").value = progressState.overallProgress;
  byId("batch-progress-label").textContent = `${progressState.overallProgress.toFixed(1)}%`;
  byId("batch-detail").textContent = progressState.currentFile
    ? `${progressState.message || "Processing"}: ${progressState.currentFile}`
    : progressState.message || "Queue idle";
  byId("batch-stats").textContent = `${progressState.completed}/${progressState.total || queueSummary.total} finished · ${progressState.succeeded} done · ${progressState.failed} failed · ${progressState.skipped} skipped`;
}

function renderQueue() {
  const queueEl = byId("queue-list");
  const summary = summarizeQueue(queue);

  if (queue.length === 0) {
    queueEl.innerHTML = `<li class="queue-empty">Add files or a folder to build a conversion run.</li>`;
  } else {
    queueEl.innerHTML = queue
      .map(
        (entry) => `
          <li class="queue-item" data-status="${entry.status}">
            <div>
              <strong>${entry.path.split(/[\\\\/]/).pop()}</strong>
              <p>${entry.path}</p>
            </div>
            <div class="queue-meta">
              <span class="pill">${entry.status}</span>
              <span>${entry.message || entry.outputPath || ""}</span>
            </div>
          </li>`,
      )
      .join("");
  }

  byId("queue-caption").textContent = `${summary.total} files in queue`;
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
  renderQueue();
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
  setStatus("Files added to queue.");
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
  setStatus(files.length ? `Added ${files.length} audio files from folder.` : "No supported audio files found in folder.", files.length ? "neutral" : "warning");
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
  setStatus("Batch conversion started.");

  try {
    await invoke("start_batch_conversion", {
      filePaths: queue.map((entry) => entry.path),
      options: {
        skipExistingOutputs: byId("skip-existing").checked,
        stopOnError: byId("stop-on-error").checked,
      },
    });
  } catch (error) {
    setRunningState(false);
    setStatus(`Unable to start batch: ${error}`, "error");
  }
}

async function cancelBatch() {
  await invoke("cancel_conversion");
  setStatus("Cancellation requested.", "warning");
}

window.addEventListener("DOMContentLoaded", async () => {
  renderQueue();

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
    setStatus("Queue cleared.");
  });
  byId("start-batch").addEventListener("click", () => {
    startBatch();
  });
  byId("cancel-batch").addEventListener("click", () => {
    cancelBatch().catch((error) => setStatus(`Cancel failed: ${error}`, "error"));
  });
});
