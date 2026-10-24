// Smoke test 4: a Settings value round-trips through get_settings/set_setting_cmd into the UI.
// Every later feature PR in the sequencing plan this harness was built for (playlist refresh
// interval, auto-reconnect behavior) adds its own Settings control, so this flow gets them cheap
// coverage for free just by extending its assertions later — it isn't tied to one particular
// setting's meaning, only to the round-trip mechanism all of them share.
import assert from "node:assert/strict";
import { By } from "selenium-webdriver";
import { closeApp, freshDataDir, launchApp, removeDataDir, startEnvironment, waitAndClick } from "./helpers.mjs";

const dataDir = freshDataDir("settings");

async function openLanToggle(driver) {
  await waitAndClick(driver, By.xpath(`//button[starts-with(@aria-label, "Open settings")]`));
  await waitAndClick(driver, By.xpath(`//nav//button[.//span[text()="Sources"]]`));
  const toggle = await driver.wait(async () => {
    const found = await driver.findElements(By.id("setting-allow-lan-sources"));
    return found.length > 0 ? found[0] : null;
  }, 10_000, "expected the LAN-sources toggle to render");
  await driver.wait(async () => await toggle.isEnabled(), 10_000, "toggle should finish loading its current value");
  return toggle;
}

const stopEnvironment = await startEnvironment(dataDir);
try {
  const driver = await launchApp();
  try {
    const toggle = await openLanToggle(driver);
    assert.equal(await toggle.getAttribute("checked"), null, "allow_lan_sources should default to off");

    await toggle.click();
    await driver.wait(async () => (await toggle.getAttribute("checked")) === "true", 10_000, "expected the checkbox to flip once clicked");

    // Round-trips through set_setting_cmd -> SQLite -> get_settings on reload, not just React
    // state in memory.
    await driver.navigate().refresh();
    const toggleAfterReload = await openLanToggle(driver);
    assert.equal(await toggleAfterReload.getAttribute("checked"), "true", "expected the saved value to survive a reload");
  } finally {
    await closeApp(driver);
  }

  console.log("smoke.settings-toggle: ok");
} finally {
  await stopEnvironment();
  removeDataDir(dataDir);
}
