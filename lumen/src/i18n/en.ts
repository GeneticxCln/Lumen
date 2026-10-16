/**
 * The English catalog, and the list of keys every other language must supply: the messages of
 * every area, put together. Add messages to the file of the area they belong to.
 *
 * Keys are dotted by screen. `{name}` is replaced by a value the caller passes. A message that
 * changes with a count has two entries, `<key>.one` and `<key>.other`, read with `tn`. A message
 * with markup writes it as `<tag>text</tag>`; the component supplies what each tag renders as.
 */
import { app } from "./messages/app.ts";
import { base } from "./messages/base.ts";
import { bulk } from "./messages/bulk.ts";
import { cast } from "./messages/cast.ts";
import { lib } from "./messages/lib.ts";
import { local } from "./messages/local.ts";
import { settings } from "./messages/settings.ts";
import { update } from "./messages/update.ts";

export const en = { ...base, ...bulk, ...cast, ...lib, ...settings, ...app, ...local, ...update } as const;
