import assert from "node:assert/strict";
import { playerShortcutFor } from "./playerKeys.ts";

assert.equal(playerShortcutFor(" "), "toggle-pause", "Space toggles playback");
assert.equal(playerShortcutFor("ArrowLeft"), "seek-back", "Left arrow seeks back");
assert.equal(playerShortcutFor("ArrowRight"), "seek-forward", "Right arrow seeks forward");
assert.equal(playerShortcutFor("9"), "volume-down", "9 lowers the volume");
assert.equal(playerShortcutFor("0"), "volume-up", "0 raises the volume");
assert.equal(playerShortcutFor("m"), "toggle-mute", "m mutes");
assert.equal(playerShortcutFor("M"), "toggle-mute", "M mutes");
assert.equal(playerShortcutFor("f"), "toggle-fullscreen", "f toggles fullscreen");
assert.equal(playerShortcutFor("F"), "toggle-fullscreen", "F toggles fullscreen");
assert.equal(playerShortcutFor("Escape"), "close", "Escape leaves the player");
assert.equal(playerShortcutFor("k"), null, "Unlisted keys are not player shortcuts");
assert.equal(playerShortcutFor("Enter"), null, "Enter is not a player shortcut");

console.log("Player keyboard checks passed; every shortcut maps to its action.");
