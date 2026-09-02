// Validates every ```mermaid block in the given Markdown files against the
// same parser GitHub uses to render them.
//
// A diagram that fails to parse shows up on GitHub as "Unable to render rich
// display" with a parse error, which is invisible from a local editor. This
// script catches that before it is pushed.
//
// Usage:
//   npm install --no-save mermaid@11 jsdom
//   node scripts/check-mermaid.mjs README.md [more.md ...]
//
// Common causes of failure, all of which have bitten this repository or are
// one keystroke away from doing so:
//   - a semicolon anywhere in a sequenceDiagram, including inside `Note over`
//     text, because mermaid reads it as a statement separator
//   - angle brackets in labels, which look like HTML
//   - a '#' in a label, which begins a mermaid entity code such as #quot;

import fs from 'node:fs';
import { JSDOM } from 'jsdom';

const dom = new JSDOM('<!DOCTYPE html><body></body>', { pretendToBeVisual: true });
global.window = dom.window;
global.document = dom.window.document;
Object.defineProperty(global, 'navigator', {
  value: dom.window.navigator,
  configurable: true,
});

const mermaid = (await import('mermaid')).default;
mermaid.initialize({ startOnLoad: false, securityLevel: 'loose' });

/** Extracts fenced mermaid blocks, remembering where each began. */
function blocksIn(path) {
  const out = [];
  let cur = null;
  fs.readFileSync(path, 'utf8')
    .split('\n')
    .forEach((line, i) => {
      if (cur === null) {
        if (/^```mermaid\s*$/.test(line)) cur = { path, start: i + 1, text: [] };
        return;
      }
      if (/^```\s*$/.test(line)) {
        out.push(cur);
        cur = null;
      } else {
        cur.text.push(line);
      }
    });
  if (cur !== null) {
    out.push({ ...cur, unterminated: true });
  }
  return out;
}

const files = process.argv.slice(2);
if (files.length === 0) {
  console.error('usage: node scripts/check-mermaid.mjs <file.md> [...]');
  process.exit(2);
}

let total = 0;
let failed = 0;

for (const file of files) {
  for (const block of blocksIn(file)) {
    total++;
    const where = `${block.path}:${block.start}`;
    if (block.unterminated) {
      failed++;
      console.error(`FAIL ${where}: mermaid block is never closed`);
      continue;
    }
    try {
      await mermaid.parse(block.text.join('\n'));
      console.log(`ok   ${where}`);
    } catch (e) {
      failed++;
      console.error(`FAIL ${where}`);
      console.error(
        String(e.message)
          .split('\n')
          .slice(0, 8)
          .map((l) => `     ${l}`)
          .join('\n'),
      );
    }
  }
}

console.log(`\n${total} mermaid diagram(s) checked, ${failed} failing`);
process.exit(failed > 0 ? 1 : 0);
