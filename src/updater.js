/**
 * Auto-update checker using Tauri's updater plugin.
 * Shows a toast notification when an update is available,
 * with download progress and automatic relaunch.
 */

let dismissed = false;

function createToast() {
  const toast = document.createElement("div");
  toast.id = "update-toast";
  toast.hidden = true;
  document.body.appendChild(toast);
  return toast;
}

function renderAvailable(toast, version, update) {
  toast.innerHTML = `
    <div class="update-toast-content">
      <div class="update-toast-icon update-toast-icon--accent">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 5v14M5 12l7 7 7-7"/></svg>
      </div>
      <div class="update-toast-body">
        <strong>Update available</strong>
        <p>Version ${version} is ready to install</p>
        <div class="update-toast-actions">
          <button id="update-install" class="update-btn update-btn--primary">Update now</button>
          <button id="update-later" class="update-btn update-btn--ghost">Later</button>
        </div>
      </div>
    </div>`;
  toast.hidden = false;

  document.getElementById("update-install").addEventListener("click", () => install(toast, update));
  document.getElementById("update-later").addEventListener("click", () => dismiss(toast));
}

function renderDownloading(toast, percent) {
  toast.innerHTML = `
    <div class="update-toast-content">
      <div class="update-toast-icon update-toast-icon--accent">
        <svg class="update-spinner" width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M12 2v4M12 18v4M4.93 4.93l2.83 2.83M16.24 16.24l2.83 2.83M2 12h4M18 12h4M4.93 19.07l2.83-2.83M16.24 7.76l2.83-2.83"/></svg>
      </div>
      <div class="update-toast-body">
        <strong>Downloading update...</strong>
        <div class="update-progress-track"><div class="update-progress-fill" style="width:${percent}%"></div></div>
        <p>${percent}%</p>
      </div>
    </div>`;
  toast.hidden = false;
}

function renderRestarting(toast) {
  toast.innerHTML = `
    <div class="update-toast-content">
      <div class="update-toast-icon update-toast-icon--success">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>
      </div>
      <div class="update-toast-body">
        <strong>Restarting...</strong>
      </div>
    </div>`;
  toast.hidden = false;
}

function renderError(toast, message) {
  toast.innerHTML = `
    <div class="update-toast-content">
      <div class="update-toast-icon update-toast-icon--error">
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><circle cx="12" cy="12" r="10"/><path d="M15 9l-6 6M9 9l6 6"/></svg>
      </div>
      <div class="update-toast-body">
        <strong>Update failed</strong>
        <p>${message}</p>
        <button id="update-dismiss" class="update-btn update-btn--ghost">Dismiss</button>
      </div>
    </div>`;
  toast.hidden = false;

  document.getElementById("update-dismiss").addEventListener("click", () => dismiss(toast));
}

function dismiss(toast) {
  dismissed = true;
  toast.hidden = true;
}

async function install(toast, update) {
  const relaunch = window.__TAURI__.process?.relaunch;

  try {
    let totalLength = 0;
    let downloaded = 0;

    await update.downloadAndInstall((event) => {
      if (event.event === "Started" && event.data.contentLength) {
        totalLength = event.data.contentLength;
        renderDownloading(toast, 0);
      } else if (event.event === "Progress") {
        downloaded += event.data.chunkLength;
        const percent = totalLength ? Math.round((downloaded / totalLength) * 100) : 0;
        renderDownloading(toast, percent);
      } else if (event.event === "Finished") {
        renderRestarting(toast);
      }
    });

    if (relaunch) await relaunch();
  } catch (err) {
    renderError(toast, err instanceof Error ? err.message : "Unknown error");
  }
}

async function checkForUpdates(toast, silent) {
  const check = window.__TAURI__.updater?.check;
  if (!check) return;

  try {
    dismissed = false;
    const update = await check();
    if (update?.available) {
      renderAvailable(toast, update.version, update);
    } else if (!silent) {
      toast.innerHTML = `
        <div class="update-toast-content">
          <div class="update-toast-icon update-toast-icon--success">
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" stroke-linejoin="round"><path d="M20 6L9 17l-5-5"/></svg>
          </div>
          <div class="update-toast-body">
            <strong>You're up to date</strong>
            <button id="update-dismiss" class="update-btn update-btn--ghost">Dismiss</button>
          </div>
        </div>`;
      toast.hidden = false;
      document.getElementById("update-dismiss").addEventListener("click", () => dismiss(toast));
    }
  } catch {
    if (!silent) {
      renderError(toast, "Could not check for updates");
    }
  }
}

export function setupUpdater() {
  const toast = createToast();

  // Auto-check silently 3 seconds after launch
  setTimeout(() => {
    if (!dismissed) checkForUpdates(toast, true);
  }, 3000);
}
