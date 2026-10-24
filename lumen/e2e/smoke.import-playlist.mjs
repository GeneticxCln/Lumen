// Smoke test 1: importing a local M3U file is the entry point to everything else — if this is
// broken, nothing downstream (favorites, playback, library browsing) is reachable either.
//
// The native "choose a file" dialog Settings → Sources → "Choose a playlist" opens cannot be
// driven by WebDriver (it lives outside the webview). This test instead calls the same
// import_playlist_file Tauri command the dialog calls, through the webview's own IPC bridge
// (invokeBackend — see helpers.mjs), then drives the real UI to confirm the imported channel
// actually shows up.
import assert from "node:assert/strict";
import { writeFileSync } from "node:fs";
import path from "node:path";
import { By } from "selenium-webdriver";
import {
  closeApp,
  freshDataDir,
  invokeBackend,
  launchApp,
  removeDataDir,
  startEnvironment,
  waitForText,
} from "./helpers.mjs";

const CHANNEL_NAME = "E2E Smoke Channel";
const dataDir = freshDataDir("import");

const stopEnvironment = await startEnvironment(dataDir);
try {
  const fixture = path.join(dataDir, "fixture.m3u");
  writeFileSync(fixture, `#EXTM3U\n#EXTINF:-1,${CHANNEL_NAME}\nhttp://example.invalid/stream.m3u8\n`);

  const driver = await launchApp();
  try {
    const result = await invokeBackend(driver, "import_playlist_file", { path: fixture });
    assert.equal(result.ok, true, `import_playlist_file failed: ${result.error}`);
    assert.equal(typeof result.value, "number", "expected a playlist id back");

    // The import happened through the raw IPC bridge, bypassing the dialog's own onSourceAdded
    // refresh callback — reload, the same as a user restarting the app, and confirm it persisted.
    await driver.navigate().refresh();
    await waitForText(driver, CHANNEL_NAME);

    const card = await driver.findElement(By.xpath(`//button[contains(@aria-label, "${CHANNEL_NAME}")]`));
    assert.ok(await card.isDisplayed(), "imported channel's card should be visible");
  } finally {
    await closeApp(driver);
  }

  console.log("smoke.import-playlist: ok");
} finally {
  await stopEnvironment();
  removeDataDir(dataDir);
}
