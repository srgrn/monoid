const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let fileInputEl;
let outputDirEl;
let filenameTemplateEl;
let overwritePolicyEl;
let convertMsgEl;
let progressMsgEl;
let outputPreviewEl;

async function selectFile() {
  try {
    const selected = await window.__TAURI__.dialog.open({
      multiple: false,
      filters: [{
        name: 'Audio Files',
        extensions: ['wav', 'mp3', 'flac', 'aac', 'ogg', 'm4a', 'mp4']
      }]
    });
    if (selected) {
      fileInputEl.value = selected;
      if (!outputDirEl.value) {
        const source = getSourceParts();
        if (source?.sourceDir) {
          outputDirEl.value = source.sourceDir;
        }
      }
      const response = await invoke("get_audio_info", { filePath: selected });
      if (response.success) {
        const info = response.data;
        document.querySelector("#audio-info").textContent = `Channels: ${info.channels}, Sample Rate: ${info.sample_rate} Hz, Bits: ${info.bits_per_sample}, Duration: ${info.duration_seconds != null ? info.duration_seconds.toFixed(2) + 's' : 'Unknown'}`;
      } else {
        document.querySelector("#audio-info").textContent = `Error reading audio info: ${response.error}`;
      }
      updateOutputPreview();
    }
   } catch (error) {
     console.error('File selection error:', error);
   }
}

async function selectOutputDir() {
  try {
    const selected = await window.__TAURI__.dialog.open({
      directory: true,
      multiple: false,
      defaultPath: outputDirEl.value || undefined,
    });
    if (selected) {
      outputDirEl.value = selected;
      updateOutputPreview();
    }
  } catch (error) {
    console.error('Output directory selection error:', error);
  }
}

function getSourceParts() {
  const filePath = fileInputEl.value.trim();
  if (!filePath) {
    return null;
  }

  const normalized = filePath.replace(/\\/g, "/");
  const segments = normalized.split("/");
  const filename = segments[segments.length - 1] || "";
  const lastDot = filename.lastIndexOf(".");
  const stem = lastDot > 0 ? filename.slice(0, lastDot) : filename;
  const originalExt = lastDot > 0 ? filename.slice(lastDot + 1) : "";
  const sourceDir = normalized.includes("/") ? normalized.slice(0, normalized.lastIndexOf("/")) : "";

  return { stem, originalExt, sourceDir };
}

function buildPreviewPath() {
  const source = getSourceParts();
  if (!source) {
    return "";
  }

  const rawTemplate = filenameTemplateEl.value.trim() || "{stem}_mono";
  const renderedName = rawTemplate
    .replaceAll("{stem}", source.stem)
    .replaceAll("{original_ext}", source.originalExt)
    .replaceAll("{ext}", "wav");
  const filename = renderedName.toLowerCase().endsWith(".wav") ? renderedName : `${renderedName}.wav`;
  const outputDir = outputDirEl.value.trim() || source.sourceDir;

  if (!outputDir) {
    return filename;
  }

  return `${outputDir.replace(/[\\/]+$/, "")}/${filename}`;
}

function updateOutputPreview() {
  const preview = buildPreviewPath();
  outputPreviewEl.textContent = preview ? `Output preview: ${preview}` : "";
}

async function convertToMono() {
  convertMsgEl.textContent = "";
  const selectFileButton = document.querySelector("#select-file");
  const selectOutputDirButton = document.querySelector("#select-output-dir");
  const convertButton = document.querySelector("#convert");
  selectFileButton.disabled = true;
  selectOutputDirButton.disabled = true;
  convertButton.disabled = true;

  try {
    await invoke("convert_to_mono", {
      request: {
        filePath: fileInputEl.value,
        outputDir: outputDirEl.value,
        filenameTemplate: filenameTemplateEl.value,
        overwritePolicy: overwritePolicyEl.value,
      },
    });
  } catch (error) {
    convertMsgEl.textContent = `Error: ${error}`;
    selectFileButton.disabled = false;
    selectOutputDirButton.disabled = false;
    convertButton.disabled = false;
  }
}

window.addEventListener("DOMContentLoaded", () => {
  fileInputEl = document.querySelector("#file-input");
  outputDirEl = document.querySelector("#output-dir");
  filenameTemplateEl = document.querySelector("#filename-template");
  overwritePolicyEl = document.querySelector("#overwrite-policy");
  convertMsgEl = document.querySelector("#convert-msg");
  progressMsgEl = document.querySelector("#progress-msg");
  outputPreviewEl = document.querySelector("#output-preview");

  listen("progress", (event) => {
    progressMsgEl.textContent = event.payload;
    if (event.payload === "Starting conversion...") {
      document.querySelector("#progress-container").style.display = "block";
      document.querySelector("#cancel-btn").style.display = "inline-block";
      document.querySelector("#progress-bar").value = 0;
    } else if (event.payload.startsWith("Conversion complete")) {
      document.querySelector("#progress-container").style.display = "none";
      document.querySelector("#cancel-btn").style.display = "none";
    } else if (event.payload.includes('%')) {
      const percent = parseFloat(event.payload);
      document.querySelector("#progress-bar").value = percent;
    }
  });

  listen("conversion-result", (event) => {
    if (event.payload.success) {
      convertMsgEl.textContent = event.payload.message;
    } else {
      convertMsgEl.textContent = `Error: ${event.payload.error}`;
    }
    document.querySelector("#progress-container").style.display = "none";
    document.querySelector("#cancel-btn").style.display = "none";
    document.querySelector("#select-file").disabled = false;
    document.querySelector("#select-output-dir").disabled = false;
    document.querySelector("#convert").disabled = false;
  });

  document.querySelector("#select-file").addEventListener("click", selectFile);
  document.querySelector("#select-output-dir").addEventListener("click", selectOutputDir);
  document.querySelector("#convert-form").addEventListener("submit", (e) => {
    e.preventDefault();
    convertToMono();
  });
  document.querySelector("#cancel-btn").addEventListener("click", () => {
    invoke("cancel_conversion");
  });
  fileInputEl.addEventListener("input", updateOutputPreview);
  outputDirEl.addEventListener("input", updateOutputPreview);
  filenameTemplateEl.addEventListener("input", updateOutputPreview);
});
