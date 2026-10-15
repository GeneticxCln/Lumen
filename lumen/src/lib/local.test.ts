import { MAX_ROOTS, MEDIA_ROOT_HINT, extensionOf, fileLabel, fileNameOf, formatClock, formatSize, isLocalPath, kindOf, mapLimit, parentOf } from "./local.ts";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }

// Names read as titles, not as filenames.
assert(fileLabel("The.Great_Film (2019).mkv") === "The Great Film (2019)", "Dots and underscores become spaces and the extension is dropped");
assert(fileLabel("no-extension") === "no-extension", "A name without an extension is left alone");
assert(fileLabel("  spaced.out.name.mp4") === "spaced out name", "Repeated separators collapse");
assert(fileNameOf("/home/quinton/Videos/clip.mp4") === "clip.mp4", "The file name is the last path segment");
assert(fileNameOf("C:\\videos\\clips\\clip.mkv") === "clip.mkv", "Windows separators are understood");

assert(extensionOf("clip.MKV") === "mkv", "Extensions compare case-insensitively");
assert(extensionOf("no-extension") === "", "A missing extension yields nothing");

assert(kindOf("clip.MKV") === "video" && kindOf("song.flac") === "audio" && kindOf("list.M3U8") === "playlist", "Media kinds are detected from the extension");
assert(kindOf("notes.txt") === "other" && kindOf("README") === "other", "Unknown extensions fall back to other");

assert(formatClock(0) === "0:00" && formatClock(65) === "1:05" && formatClock(3725) === "1:02:05", "Durations format with hours only when they have them");
assert(formatClock(-5) === "0:00" && formatClock(59.9) === "0:59", "Negative and fractional positions clamp");

assert(formatSize(undefined) === "" && formatSize(512) === "512 B" && formatSize(2048) === "2 KB", "Small sizes read as B and KB");
assert(formatSize(5 * 1024 * 1024) === "5.0 MB" && formatSize(3 * 1024 * 1024 * 1024) === "3.00 GB", "Large sizes read as MB and GB");

assert(isLocalPath("/home/quinton/Videos/clip.mp4"), "An absolute path is local");
assert(isLocalPath("C:\\videos\\clip.mp4"), "A Windows path is local");
assert(!isLocalPath("https://example.org/live.m3u8") && !isLocalPath("rtsp://example.org/live"), "Stream URLs are not local");
assert(!isLocalPath(""), "An empty path is not local");

assert(parentOf("/home/quinton/Videos") === "/home/quinton", "POSIX parents resolve");
assert(parentOf("/home/quinton/Videos/") === "/home/quinton", "Trailing separators are ignored");
assert(parentOf("C:\\videos\\clips") === "C:\\videos", "Windows parents resolve");
assert(parentOf("/") === "", "The root has no parent, so Up is disabled there");

assert(MEDIA_ROOT_HINT.test("Videos") && MEDIA_ROOT_HINT.test("downloads") && MEDIA_ROOT_HINT.test("Movies"), "Usual media folders are recognised first");
assert(!MEDIA_ROOT_HINT.test("Documents") && !MEDIA_ROOT_HINT.test("Projects"), "Unrelated folders are not media roots");
assert(MAX_ROOTS > 0 && MAX_ROOTS <= 8, "A scan reads a bounded number of top-level folders");

// The scan must never open every folder at once, and must keep the caller's order.
let inFlight = 0;
let peak = 0;
const order: number[] = [];
const results = await mapLimit([1, 2, 3, 4, 5, 6, 7], 3, async (value) => {
  inFlight += 1;
  peak = Math.max(peak, inFlight);
  await new Promise((resolve) => setTimeout(resolve, value % 2 === 0 ? 1 : 5));
  inFlight -= 1;
  order.push(value);
  return value * 2;
});
assert(peak <= 3, `Directory reads stay within the concurrency limit (saw ${peak})`);
assert(results.join(",") === "2,4,6,8,10,12,14", "Results keep the input order regardless of completion order");
assert(order.length === 7, "Every item is visited exactly once");
const empty = await mapLimit([], 3, async () => 0);
assert(empty.length === 0, "An empty scan resolves without starting a worker");

console.log("Local media checks passed; labels, durations, paths and bounded folder scanning behave.");
