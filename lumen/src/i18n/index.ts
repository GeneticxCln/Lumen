import { en } from "./en.ts";

export type MessageKey = keyof typeof en;

/** What a translation must provide: every key the English catalog has. */
export type Catalog = Record<MessageKey, string>;

/** The `{name}` placeholders in a message type, as a union of names. */
type Placeholders<S extends string> = S extends `${string}{${infer Name}}${infer Rest}` ? Name | Placeholders<Rest> : never;

type Values<S extends string> = Record<Placeholders<S>, string | number>;

/** Passes `[values]` only for messages that have placeholders, so a missing value is a compile error. */
type VarArgs<S extends string> = [Placeholders<S>] extends [never] ? [] : [values: Values<S>];

/** Keys whose message has no `{placeholders}`. */
export type PlainKey = {
  [K in MessageKey]: [Placeholders<(typeof en)[K]>] extends [never] ? K : never;
}[MessageKey];

/** `foo` for every pair `foo.one` / `foo.other`. */
export type PluralKey = {
  [K in MessageKey]: K extends `${infer Base}.other` ? Base : never;
}[MessageKey];

const locales: Record<string, Catalog> = { en };
let active: Catalog = en;
let activeCode = "en";

/** Adds a language. The type makes a missing key a compile error. */
export function addLocale(code: string, catalog: Catalog): void {
  locales[code.toLowerCase()] = catalog;
}

/** The languages that can be selected. */
export function availableLocales(): string[] {
  return Object.keys(locales);
}

/** Selects a language by code (`de`, `pt-BR`); `de-AT` falls back to `de`, anything unknown to English. */
export function setLocale(code: string): void {
  const wanted = code.toLowerCase();
  const found = locales[wanted] ? wanted : wanted.split("-")[0];
  active = locales[found] ?? en;
  activeCode = locales[found] ? found : "en";
}

function fill(message: string, values?: Record<string, string | number>): string {
  if (!values) return message;
  return message.replace(/\{(\w+)\}/g, (whole, name: string) => (name in values ? String(values[name]) : whole));
}

function lookup(key: MessageKey): string {
  return active[key] ?? en[key];
}

export function t<K extends MessageKey>(key: K, ...args: VarArgs<(typeof en)[K]>): string {
  return fill(lookup(key), args[0] as Record<string, string | number> | undefined);
}

/** The `one` or `other` form of a message for `count`, which is also available as `{count}`. */
export function tn(base: PluralKey, count: number, values?: Record<string, string | number>): string {
  const category = new Intl.PluralRules(activeCode).select(count);
  // Languages with more forms (`few`, `many`) add keys and a branch here; English has two.
  const key = (category === "one" ? `${base}.one` : `${base}.other`) as MessageKey;
  return fill(lookup(key), { ...values, count });
}

/** A piece of a message that has markup: plain text, or text inside a `<tag>`. */
export type RichPart = { text: string; tag?: string };

/** Splits `see <code>x</code> and <code>y</code>` into text and tagged parts. Tags do not nest. */
export function splitRich(message: string): RichPart[] {
  const parts: RichPart[] = [];
  const pattern = /<(\w+)>(.*?)<\/\1>/g;
  let last = 0;
  for (const match of message.matchAll(pattern)) {
    if (match.index > last) parts.push({ text: message.slice(last, match.index) });
    parts.push({ tag: match[1], text: match[2] });
    last = match.index + match[0].length;
  }
  if (last < message.length) parts.push({ text: message.slice(last) });
  return parts;
}

/** The message with its markup split out, for a component to render. */
export function tRich<K extends MessageKey>(key: K, ...args: VarArgs<(typeof en)[K]>): RichPart[] {
  return splitRich(t(key, ...args));
}
