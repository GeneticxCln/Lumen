// Shared plumbing for the E2E smoke tests. Each test script calls startEnvironment() once
// (Xvfb + the Vite dev server behind tauri.conf.json's devUrl + tauri-driver), then launchApp()
// for each app instance it needs (a fresh, isolated XDG_DATA_HOME per call unless a dataDir is
// passed in, so a test can relaunch against the same data — see smoke.favorite.mjs).
import { execFileSync, spawn } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { Builder, By } from "selenium-webdriver";
import { Capabilities } from "selenium-webdriver/lib/capabilities.js";

const here = path.dirname(fileURLToPath(import.meta.url));
const repoRoot = path.join(here, "..");
export const binaryPath = path.join(repoRoot, "target", "debug", "lumen");

const DISPLAY = ":97";
const VITE_PORT = 1420; // must match tauri.conf.json's "devUrl"
const DRIVER_PORT = 4545;
const DRIVER_NATIVE_PORT = 4546;

/** Captures a spawned child's stdout/stderr so a startup failure or timeout can report *why*
 * instead of just that it didn't happen in time — piped-but-unread output is otherwise silently
 * discarded, which is exactly what made the first real CI run of this harness unnecessarily hard
 * to diagnose (a bare "timed out waiting for http://127.0.0.1:1420" with no clue why). */
function captureOutput(child) {
  const chunks = [];
  const collect = (chunk) => chunks.push(chunk.toString());
  child.stdout?.on("data", collect);
  child.stderr?.on("data", collect);
  return () => chunks.join("");
}

function waitForHttp(url, timeoutMs, describeFailure) {
  const deadline = Date.now() + timeoutMs;
  const attempt = async () => {
    try {
      const res = await fetch(url);
      await res.body?.cancel();
      return;
    } catch {
      if (Date.now() > deadline) {
        const extra = describeFailure?.();
        throw new Error(
          `timed out waiting for ${url}` + (extra ? `\n--- output so far ---\n${extra}` : ""),
        );
      }
      await new Promise((resolve) => setTimeout(resolve, 500));
      return attempt();
    }
  };
  return attempt();
}

function waitForExit(child) {
  return new Promise((resolve) => child.once("exit", resolve));
}

/**
 * Starts Xvfb, the Vite dev server and tauri-driver, the last pinned to `dataDir` via
 * XDG_DATA_HOME so the app resolves its app_data_dir() under that throwaway directory instead
 * of the real one — with no application code changes needed. Every app launchApp() starts for
 * the rest of this environment's life (one script may call launchApp()/closeApp() more than
 * once — see smoke.favorite.mjs — to relaunch against the same data) inherits that same
 * directory, since tauri-driver spawns each app with its own fixed environment. Returns
 * `stopEnvironment()`, which must be called (ideally from a `finally`) once done.
 */
export async function startEnvironment(dataDir, extraEnv = {}) {
  // Spawned directly (not via `npm run dev`) so stopEnvironment()'s kill() actually reaches the
  // real vite process instead of orphaning it under an already-dead npm wrapper.
  const vite = spawn(path.join(repoRoot, "node_modules", ".bin", "vite"), ["--strictPort"], {
    cwd: repoRoot,
    stdio: ["ignore", "pipe", "pipe"],
  });
  const viteOutput = captureOutput(vite);
  const xvfb = spawn("Xvfb", [DISPLAY, "-screen", "0", "1280x800x24"], { stdio: ["ignore", "pipe", "pipe"] });

  // A cold CI runner right after a multi-minute cargo build is slower to get Vite's dev server
  // listening than this harness ever saw locally; give it real headroom rather than guessing.
  await waitForHttp(`http://127.0.0.1:${VITE_PORT}`, 90_000, viteOutput);

  const driver = spawn(
    "tauri-driver",
    ["--port", String(DRIVER_PORT), "--native-port", String(DRIVER_NATIVE_PORT)],
    { env: { ...process.env, DISPLAY, XDG_DATA_HOME: dataDir, ...extraEnv }, stdio: ["ignore", "pipe", "pipe"] },
  );
  // tauri-driver has no health endpoint; give WebKitWebDriver a moment to come up behind it.
  await new Promise((resolve) => setTimeout(resolve, 1500));

  return async function stopEnvironment() {
    vite.kill();
    driver.kill();
    xvfb.kill();
    await Promise.all([waitForExit(vite), waitForExit(driver), waitForExit(xvfb)]);
  };
}

/** A fresh, empty directory under the system temp dir, for XDG_DATA_HOME. */
export function freshDataDir(name) {
  return mkdtempSync(path.join(tmpdir(), `lumen-e2e-${name}-`));
}

/** Launches the app (against whatever data dir startEnvironment() was given) and returns a
 * connected WebDriver session. */
export async function launchApp() {
  const caps = new Capabilities();
  caps.set("tauri:options", { application: binaryPath, args: [] });
  caps.set("browserName", "wry");

  const driver = await new Builder()
    .withCapabilities(caps)
    .usingServer(`http://127.0.0.1:${DRIVER_PORT}/`)
    .build();
  // A non-empty <title> is already true from the static index.html before React has mounted
  // anything, so it is not enough on its own — wait for the app shell's own nav to actually
  // render before handing the session back to the caller.
  await driver.wait(async () => (await driver.getTitle()).length > 0, 20_000);
  await driver.wait(async () => (await driver.findElements(By.css("nav.main-nav"))).length > 0, 20_000);
  return driver;
}

/** Ends the WebDriver session and makes sure the underlying app process is gone either way,
 * waiting for it to actually exit so a caller's cleanup (e.g. removeDataDir) doesn't race its
 * shutdown (WebKit keeps writing to its own cache directory for a moment after being killed). */
export async function closeApp(driver) {
  try {
    await driver.quit();
  } finally {
    try {
      execFileSync("pkill", ["-9", "-f", binaryPath]);
    } catch {
      // Already gone — pkill exits non-zero when nothing matched, which is the common case.
    }
    for (let i = 0; i < 20; i++) {
      try {
        execFileSync("pgrep", ["-f", binaryPath]);
        await new Promise((resolve) => setTimeout(resolve, 200));
      } catch {
        break; // pgrep exits non-zero once nothing matches.
      }
    }
  }
}

/** Polls until `document.body.innerText` contains `text`, or throws after `timeoutMs`. */
export async function waitForText(driver, text, timeoutMs = 20_000) {
  await driver.wait(async () => {
    const body = await driver.executeScript("return document.body.innerText");
    return body.includes(text);
  }, timeoutMs, `expected to find text "${text}" on screen`);
}

/**
 * Waits for `locator` to match an element and clicks it, instead of a bare findElement().click()
 * — a navigation or state change (opening Settings, switching section) can take a render pass
 * or two to reach the webview, and a bare findElement() right after triggering one is a real,
 * observed source of flaky NoSuchElementError under this harness's own test load (fine in
 * isolation, intermittent once several environment start/stop cycles have already run).
 */
export async function waitAndClick(driver, locator, timeoutMs = 10_000) {
  const element = await driver.wait(async () => {
    const found = await driver.findElements(locator);
    return found.length > 0 ? found[0] : null;
  }, timeoutMs, `expected an element matching ${locator} to click`);
  await element.click();
  return element;
}

/**
 * Calls a Tauri command directly through the webview's injected IPC bridge. Used only for the
 * one step a native OS dialog would otherwise block from WebDriver (picking a file to import) —
 * every other step in these tests drives the real UI.
 */
export async function invokeBackend(driver, cmd, args) {
  return driver.executeAsyncScript(
    `const [cmd, args, callback] = arguments;
     window.__TAURI_INTERNALS__.invoke(cmd, args)
       .then((value) => callback({ ok: true, value }))
       .catch((error) => callback({ ok: false, error: String(error) }));`,
    cmd,
    args,
  );
}

/**
 * The app process can still be releasing its own files (WebKit's shader/disk cache in
 * particular) for a brief moment after pkill returns, which turns a plain rmSync into a race —
 * maxRetries/retryDelay is Node's built-in handling for exactly this class of transient
 * ENOTEMPTY/EBUSY failure. Deliberately swallows whatever is left after that: this is best-
 * effort temp-dir hygiene, and a leftover directory under the system temp dir is harmless,
 * while letting a cleanup failure escape a `finally` block would replace — and hide — whatever
 * real assertion failure the test itself hit.
 */
export function removeDataDir(dataDir) {
  try {
    rmSync(dataDir, { recursive: true, force: true, maxRetries: 5, retryDelay: 200 });
  } catch (error) {
    console.warn(`(cleanup) could not fully remove ${dataDir}: ${error.message}`);
  }
}
