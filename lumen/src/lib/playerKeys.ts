export type PlayerShortcut =
  | "toggle-pause"
  | "seek-back"
  | "seek-forward"
  | "volume-down"
  | "volume-up"
  | "toggle-mute"
  | "toggle-fullscreen"
  | "close";

export const PLAYER_SEEK_STEP_SECONDS = 10;
export const PLAYER_VOLUME_STEP = 5;

/** Returns the player action for a key, or null when the key is not a player shortcut. */
export function playerShortcutFor(key: string): PlayerShortcut | null {
  switch (key) {
    case " ":
      return "toggle-pause";
    case "ArrowLeft":
      return "seek-back";
    case "ArrowRight":
      return "seek-forward";
    case "9":
      return "volume-down";
    case "0":
      return "volume-up";
    case "m":
    case "M":
      return "toggle-mute";
    case "f":
    case "F":
      return "toggle-fullscreen";
    case "Escape":
      return "close";
    default:
      return null;
  }
}
