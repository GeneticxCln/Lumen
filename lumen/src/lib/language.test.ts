import { matchesLanguage } from "./language.ts";

function assert(value: unknown, message: string): asserts value { if (!value) throw new Error(message); }

// With no choice, every result passes.
assert(matchesLanguage({ language: "fr" }, ""), "No chosen language keeps every result");
assert(matchesLanguage({ language: null }, ""), "No chosen language keeps unlabeled results");

// External results must match the chosen language exactly.
assert(matchesLanguage({ language: "en" }, "en"), "A matching external result is kept");
assert(!matchesLanguage({ language: "fr" }, "en"), "A different external result is hidden");
assert(!matchesLanguage({ language: null }, "en"), "An unlabeled external result is hidden");
assert(!matchesLanguage({ language: undefined }, "en"), "An external result without a language field is hidden");
assert(!matchesLanguage({ language: "" }, "en"), "An empty language label is hidden");

// Library items follow their tvg-language label when they have one; unlabeled items are kept.
assert(matchesLanguage({ language: null, channel_id: 1 }, "en"), "An unlabeled library item is kept");
assert(matchesLanguage({ language: undefined, channel_id: 7 }, "fr"), "A library item with no language is kept");
assert(matchesLanguage({ language: "fr", channel_id: 3 }, "fr"), "A library item in the chosen language is kept");
assert(!matchesLanguage({ language: "fr", channel_id: 3 }, "en"), "A library item in another language is hidden");
