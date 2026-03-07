export function normalizePaths(paths) {
  const seen = new Set();
  const normalized = [];

  for (const path of paths) {
    if (typeof path !== "string") {
      continue;
    }

    const trimmed = path.trim();
    if (!trimmed || seen.has(trimmed)) {
      continue;
    }

    seen.add(trimmed);
    normalized.push(trimmed);
  }

  return normalized;
}

export function mergeQueue(existingEntries, newPaths) {
  const existingPaths = new Set(existingEntries.map((entry) => entry.path));
  const merged = [...existingEntries];

  for (const path of normalizePaths(newPaths)) {
    if (existingPaths.has(path)) {
      continue;
    }

    merged.push({
      path,
      status: "ready",
      message: "",
      outputPath: "",
    });
    existingPaths.add(path);
  }

  return merged;
}

export function summarizeQueue(entries) {
  return entries.reduce(
    (summary, entry) => {
      summary.total += 1;
      if (entry.status === "done") summary.done += 1;
      if (entry.status === "failed") summary.failed += 1;
      if (entry.status === "skipped") summary.skipped += 1;
      if (entry.status === "processing") summary.processing += 1;
      if (entry.status === "cancelled") summary.cancelled += 1;
      if (entry.status === "ready") summary.ready += 1;
      return summary;
    },
    {
      total: 0,
      ready: 0,
      processing: 0,
      done: 0,
      failed: 0,
      skipped: 0,
      cancelled: 0,
    },
  );
}
