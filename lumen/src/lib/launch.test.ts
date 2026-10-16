import assert from "node:assert/strict";
import { launchNotice, planLaunch } from "./launch.ts";
import type { OpenTarget } from "./launch.ts";

const media = (path: string): OpenTarget => ({ kind: "media", path });
const stream = (url: string): OpenTarget => ({ kind: "stream", url });

assert.deepEqual(planLaunch([]), { play: null, playlists: [], subtitles: [], skipped: 0 });
assert.deepEqual(planLaunch([media("/a.mkv"), { kind: "subtitle", path: "/a.srt" }]), { play: { kind: "media", path: "/a.mkv" }, playlists: [], subtitles: ["/a.srt"], skipped: 0 });
assert.equal(planLaunch([stream("https://x.example/a.m3u8"), media("/b.mp4")]).play?.kind, "media", "a file wins over a web address");
assert.equal(planLaunch([stream("https://x.example/a.m3u8")]).play?.kind, "stream");
assert.equal(planLaunch([media("/a.mkv"), media("/b.mkv"), stream("https://x.example/c")]).skipped, 2);
assert.deepEqual(planLaunch([{ kind: "playlist", path: "/l.m3u" }, { kind: "playlist", path: "/m.m3u8" }]).playlists, ["/l.m3u", "/m.m3u8"]);

assert.equal(launchNotice(planLaunch([media("/a.mkv")]), 0, false), null);
assert.equal(launchNotice(planLaunch([]), 2, false), "Added 2 playlists to your sources.");
assert.equal(launchNotice(planLaunch([]), 1, false), "Added 1 playlist to your sources.");
assert.match(launchNotice(planLaunch([media("/a"), media("/b")]), 0, false)!, /1 other was not opened/);
assert.match(launchNotice(planLaunch([{ kind: "subtitle", path: "/a.srt" }]), 0, true)!, /Open a video first/);

console.log("Launch checks passed; one item plays, playlists import, subtitles follow the video.");
