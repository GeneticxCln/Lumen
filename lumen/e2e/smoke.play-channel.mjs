// Smoke test 2: playing a local media file. The only thing in this suite that exercises a real
// mpv spawn-and-control cycle under Xvfb — nothing else here touches playback at all.
//
// Local Device's folder browser lists the real $HOME (there's no free-text path field in the
// UI), so this test points HOME at its own throwaway directory containing just the fixture
// video, generated on the fly with ffmpeg rather than committing a binary to the repo.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import path from "node:path";
import { By } from "selenium-webdriver";
import {
  closeApp,
  freshDataDir,
  launchApp,
  removeDataDir,
  startEnvironment,
  waitAndClick,
  waitForText,
} from "./helpers.mjs";

const dataDir = freshDataDir("play-data");
const homeDir = freshDataDir("play-home");
const videoName = "e2e-smoke-clip.mp4";

execFileSync("ffmpeg", [
  "-y", "-loglevel", "error", "-f", "lavfi", "-i", "color=c=blue:s=320x240:d=2",
  "-f", "lavfi", "-i", "anullsrc=r=44100:cl=mono",
  "-shortest", "-pix_fmt", "yuv420p",
  path.join(homeDir, videoName),
]);

const stopEnvironment = await startEnvironment(dataDir, { HOME: homeDir });
try {
  const driver = await launchApp();
  try {
    await waitAndClick(driver, By.xpath(`//nav[@aria-label="Main navigation"]//button[.//span[text()="Local Device"]]`));

    // The home directory scan runs on load and finds the fixture clip as the page's single
    // "feature" — its own Play button, no folder-browser navigation needed.
    const fileLabel = videoName.replace(/\.[^.]+$/, "");
    await waitForText(driver, fileLabel);
    // "Play" sits as a bare text node next to the button's icon, not inside its own element, so
    // this needs normalize-space() over the whole button rather than a child-element predicate.
    await waitAndClick(driver, By.xpath(`//button[normalize-space()="Play"]`));

    await driver.wait(async () => {
      const elements = await driver.findElements(By.css('[aria-label="Now playing"]'));
      return elements.length > 0;
    }, 20_000, "expected the now-playing dock to appear after clicking the file");

    const body = await driver.executeScript("return document.body.innerText");
    assert.ok(!body.includes("Couldn't play this"), `playback reported a failure: ${body}`);
  } finally {
    await closeApp(driver);
  }

  console.log("smoke.play-channel: ok");
} finally {
  await stopEnvironment();
  removeDataDir(dataDir);
  removeDataDir(homeDir);
}
