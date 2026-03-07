import test from "node:test";
import assert from "node:assert/strict";

import { mergeQueue, normalizePaths, summarizeQueue } from "./queue.js";

test("normalizePaths removes blanks and duplicates while preserving order", () => {
  assert.deepEqual(normalizePaths([" /tmp/a.wav ", "", "/tmp/a.wav", "/tmp/b.mp3"]), [
    "/tmp/a.wav",
    "/tmp/b.mp3",
  ]);
});

test("mergeQueue appends only unseen file paths", () => {
  const queue = mergeQueue(
    [{ path: "/tmp/a.wav", status: "done", message: "", outputPath: "/tmp/a_mono.wav" }],
    ["/tmp/a.wav", "/tmp/b.wav"],
  );

  assert.equal(queue.length, 2);
  assert.deepEqual(queue[1], {
    path: "/tmp/b.wav",
    status: "ready",
    message: "",
    outputPath: "",
  });
});

test("summarizeQueue counts each queue state", () => {
  assert.deepEqual(
    summarizeQueue([
      { status: "ready" },
      { status: "processing" },
      { status: "done" },
      { status: "failed" },
      { status: "skipped" },
      { status: "cancelled" },
    ]),
    {
      total: 6,
      ready: 1,
      processing: 1,
      done: 1,
      failed: 1,
      skipped: 1,
      cancelled: 1,
    },
  );
});
