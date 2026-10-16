// Lists the hard-coded UI text in the given component files, with line numbers:
//   npm run i18n:hits -- src/components/SettingsPage.tsx
import { readFileSync } from "node:fs";
import { findHardCodedText } from "./scan.ts";

const files = process.argv.slice(2);
if (files.length === 0) {
  console.log("usage: npm run i18n:hits -- <file.tsx> [...]");
  process.exit(1);
}
for (const file of files) {
  const hits = findHardCodedText(readFileSync(file, "utf8"), file);
  console.log(`${file}: ${hits.length} hard-coded string${hits.length === 1 ? "" : "s"}`);
  for (const hit of hits) console.log(`  ${hit.line}: ${JSON.stringify(hit.text)}`);
}
