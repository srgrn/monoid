const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let fileInputEl;
let convertMsgEl;
let progressMsgEl;

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
      const response = await invoke("get_audio_info", { filePath: selected });
      if (response.success) {
        const info = response.data;
        document.querySelector("#audio-info").textContent = `Channels: ${info.channels}, Sample Rate: ${info.sample_rate} Hz, Bits: ${info.bits_per_sample}, Duration: ${info.duration_seconds != null ? info.duration_seconds.toFixed(2) + 's' : 'Unknown'}`;
      } else {
        document.querySelector("#audio-info").textContent = `Error reading audio info: ${response.error}`;
      }
    }
   } catch (error) {
     console.error('File selection error:', error);
   }
 }

async function convertToMono() {
  convertMsgEl.textContent = "";
  document.querySelector("#select-file").disabled = true;
  document.querySelector("#convert").disabled = true;
  invoke("convert_to_mono", { filePath: fileInputEl.value });
}

window.addEventListener("DOMContentLoaded", () => {
  fileInputEl = document.querySelector("#file-input");
  convertMsgEl = document.querySelector("#convert-msg");
  progressMsgEl = document.querySelector("#progress-msg");

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
    document.querySelector("#convert").disabled = false;
  });

  document.querySelector("#select-file").addEventListener("click", selectFile);
  document.querySelector("#convert-form").addEventListener("submit", (e) => {
    e.preventDefault();
    convertToMono();
  });
  document.querySelector("#cancel-btn").addEventListener("click", () => {
    invoke("cancel_conversion");
  });
});
