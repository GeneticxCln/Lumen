// Smoke test 3: favoriting a channel, then relaunching the app against the same data,
// proves the full SQLite round-trip across a real restart — not just in-memory React state
// surviving a re-render.
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

const CHANNEL_NAME = "E2E Favorite Channel";
const dataDir = freshDataDir("favorite");

const stopEnvironment = await startEnvironment(dataDir);
try {
  const fixture = path.join(dataDir, "fixture.m3u");
  writeFileSync(fixture, `#EXTM3U\n#EXTINF:-1,${CHANNEL_NAME}\nhttp://example.invalid/stream.m3u8\n`);

  // First launch: import the fixture (via the IPC bridge — see smoke.import-playlist.mjs for
  // why) and favorite it through the real UI.
  let driver = await launchApp();
  try {
    const result = await invokeBackend(driver, "import_playlist_file", { path: fixture });
    assert.equal(result.ok, true, `import_playlist_file failed: ${result.error}`);

    await driver.navigate().refresh();
    await waitForText(driver, CHANNEL_NAME);

    const favoriteButton = await driver.findElement(By.xpath(`//button[contains(@aria-label, "${CHANNEL_NAME}")][@aria-pressed]`));
    assert.equal(await favoriteButton.getAttribute("aria-pressed"), "false", "should start un-favorited");
    await favoriteButton.click();
    await driver.wait(async () => (await favoriteButton.getAttribute("aria-pressed")) === "true", 10_000, "expected aria-pressed to flip after clicking favorite");
  } finally {
    await closeApp(driver);
  }

  // Second launch, same data directory: the favorite must still be there.
  driver = await launchApp();
  try {
    await waitForText(driver, CHANNEL_NAME);
    const favoriteButton = await driver.findElement(By.xpath(`//button[contains(@aria-label, "${CHANNEL_NAME}")][@aria-pressed]`));
    assert.equal(await favoriteButton.getAttribute("aria-pressed"), "true", "favorite should have persisted across a relaunch");
  } finally {
    await closeApp(driver);
  }

  console.log("smoke.favorite: ok");
} finally {
  await stopEnvironment();
  removeDataDir(dataDir);
}
