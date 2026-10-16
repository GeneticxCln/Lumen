import { errorMessage, errorText } from "./errors.ts";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }

// The shapes Rust actually sends when a Tauri command fails, one per PlayerError variant.
const rustErrors: Record<string, string> = {
  Mpv: "mpv error: mpv IPC command timed out",
  Io: "I/O error: No such file or directory (os error 2)",
  Json: "JSON error: expected value at line 1 column 1",
  Sql: "SQL error: no such table: channels",
  Http: "HTTP error: HTTP 500 when fetching playlist",
  Parse: "Parse error: Join error during M3U parse",
  NotFound: "Not found: Channel no longer exists",
  InvalidArg: "Invalid argument: That PIN is not correct",
};

for (const [variant, expected] of Object.entries(rustErrors)) {
  const serialized = { [variant]: expected };
  assert(errorMessage(serialized) === expected, `A serialized ${variant} error renders its message, not "[object Object]"`);
  assert(errorText(serialized) === expected, `errorText unwraps ${variant}`);
  assert(!errorMessage(serialized).includes("object"), `A serialized ${variant} error never renders as an object`);
}

const jsError = new Error("plain failure");
assert(errorMessage(jsError) === "plain failure", "A real Error keeps its message");
assert(errorText(jsError) === "plain failure", "errorText reads a real Error");
assert(errorMessage("already a string") === "already a string", "A string rejection passes through");
assert(errorMessage("") === "", "An empty string is preserved");

// Shapes with nothing to extract must not be mistaken for a message.
assert(errorText(null) === null, "null has no message");
assert(errorText(undefined) === null, "undefined has no message");
assert(errorText({}) === null, "an empty object has no message");
assert(errorText({ a: "1", b: "2" }) === null, "a multi-field object has no single message");
assert(errorText({ Struct: { inner: "nested" } }) === null, "a nested object has no readable message");
assert(errorMessage(null) === "null", "errorMessage still returns something printable");
assert(errorText(42) === null, "a number has no message");

// The fallback idiom used by components that have their own wording for unknown shapes.
const formatErr = (failure: unknown, fallback: string) => errorText(failure) ?? fallback;
assert(formatErr({ InvalidArg: "That PIN is not correct" }, "Something went wrong") === "That PIN is not correct", "Callers keep their fallback only for unreadable shapes");
assert(formatErr({ unexpected: { deep: true } }, "Something went wrong") === "Something went wrong", "Unreadable shapes use the caller's fallback");

console.log("Error-message regression checks passed; Rust error variants unwrap to readable text.");
