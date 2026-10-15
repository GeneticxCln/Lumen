/**
 * Every Tauri command returns `Result<T, PlayerError>`, and Tauri rejects the JS promise with
 * the *serialized* `PlayerError` rather than an `Error`. A `PlayerError::Sql("…")` arrives as
 * `{ Sql: "…" }`, so the app-wide idiom
 * `failure instanceof Error ? failure.message : String(failure)` renders "[object Object]"
 * for essentially every backend failure. These two helpers are the shared replacement.
 */

/** The message inside a rejection, or null when its shape says nothing useful. */
export function errorText(failure: unknown): string | null {
  if (failure instanceof Error) return failure.message;
  if (typeof failure === "string") return failure;
  if (failure && typeof failure === "object") {
    // A serialized enum error: exactly one field holding the message, e.g. { Sql: "no such table" }.
    const values = Object.values(failure as Record<string, unknown>);
    if (values.length === 1 && typeof values[0] === "string") return values[0];
  }
  return null;
}

/** The same, always printable: unwraps a serialized backend error and never returns "[object Object]"
 * for the shapes we actually receive, while still degrading to `String(failure)` for anything else. */
export function errorMessage(failure: unknown): string {
  return errorText(failure) ?? String(failure);
}
