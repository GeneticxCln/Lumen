import assert from "node:assert/strict";
import { readdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname, join, relative } from "node:path";
import { fileURLToPath } from "node:url";
import { en } from "./en.ts";
import { findHardCodedText, stringLiterals } from "./scan.ts";

// What the scanner counts. It has to be right for the gate below to mean anything.
const sample = (source: string) => findHardCodedText(source).map((hit) => hit.text);

assert.deepEqual(sample(`const A = () => <p>Hello there</p>;`), ["Hello there"]);
assert.deepEqual(sample(`const A = () => <p>{"Hello"}</p>;`), ["Hello"]);
assert.deepEqual(sample(`const A = () => <button aria-label="Close" title={ok ? "Yes" : "No"} />;`), ["Close", "Yes", "No"]);
assert.deepEqual(sample("const A = () => <p>{`Unlock ${name}`}</p>;"), ["Unlock"]);
assert.deepEqual(sample(`const A = () => <p>{busy && "Working…"}</p>;`), ["Working…"]);
assert.deepEqual(sample(`const A = () => <p>{error ?? "Failed"}</p>;`), ["Failed"]);
// Not user-visible text, or already from the catalog.
assert.deepEqual(sample(`const A = () => <div className="is-open" data-testid="x" id="a-b">{t("sync.title")}</div>;`), []);
assert.deepEqual(sample(`const A = () => <p>{count}% · {" "}{"\\u00b7"}</p>;`), []);
assert.deepEqual(sample(`const A = () => <p>{name}</p>;`), []);
assert.deepEqual(sample(`const A = () => <input type="text" value={size} />;`), []);
assert.equal(findHardCodedText(`const A = () => (\n  <p>\n    Hi\n  </p>\n);`)[0].line, 3);

// The gate: the text hard-coded in each component may only go down.
const here = dirname(fileURLToPath(import.meta.url));
const srcDir = join(here, "..");
const baselinePath = join(here, "baseline.json");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const path = join(dir, entry.name);
    if (entry.isDirectory()) return sourceFiles(path);
    return /\.tsx?$/.test(entry.name) && !/\.test\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

const posix = (path: string) => relative(srcDir, path).split("\\").join("/");
const files = sourceFiles(srcDir);
const counts = new Map<string, { count: number; hits: ReturnType<typeof findHardCodedText> }>();
for (const path of files.filter((file) => file.endsWith(".tsx"))) {
  const hits = findHardCodedText(readFileSync(path, "utf8"), path);
  if (hits.length > 0) counts.set(posix(path), { count: hits.length, hits });
}

if (process.argv.includes("--update")) {
  const next = Object.fromEntries([...counts].sort(([a], [b]) => a.localeCompare(b)).map(([file, { count }]) => [file, count]));
  writeFileSync(baselinePath, `${JSON.stringify(next, null, 2)}\n`);
  console.log(`gate.test.ts: baseline written (${Object.keys(next).length} files, ${Object.values(next).reduce((a, b) => a + b, 0)} strings)`);
  process.exit(0);
}

const baseline: Record<string, number> = JSON.parse(readFileSync(baselinePath, "utf8"));
const advice = "Put new text in src/i18n/en.ts and show it with t().";
const problems: string[] = [];

for (const [file, { count, hits }] of counts) {
  const allowed = baseline[file] ?? 0;
  if (count > allowed) {
    const lines = hits.map((hit) => `    ${file}:${hit.line}  "${hit.text}"`).join("\n");
    problems.push(`${file} has ${count} hard-coded strings but ${allowed} are allowed.\n${lines}`);
  }
}
for (const [file, allowed] of Object.entries(baseline)) {
  const count = counts.get(file)?.count ?? 0;
  if (count < allowed) problems.push(`${file} has ${count} hard-coded strings and the baseline says ${allowed}. Run npm run i18n:baseline to record the progress.`);
}
assert.equal(problems.length, 0, `\nHard-coded UI text:\n${problems.join("\n")}\n${advice}\n`);

// The catalog: every message is used by something. A leftover key is a string nobody sees.
const referenced = new Set<string>();
for (const path of files) {
  if (posix(path) === "i18n/en.ts") continue;
  for (const literal of stringLiterals(readFileSync(path, "utf8"), path)) referenced.add(literal);
}
const unused = Object.keys(en).filter((key) => {
  const base = key.replace(/\.(one|other)$/, "");
  return !referenced.has(key) && !(base !== key && referenced.has(base));
});
assert.deepEqual(unused, [], `Catalog messages nothing uses: ${unused.join(", ")}`);

const total = [...counts.values()].reduce((sum, entry) => sum + entry.count, 0);
console.log(`gate.test.ts: ok (${Object.keys(en).length} messages, ${total} strings still to migrate in ${counts.size} files)`);
