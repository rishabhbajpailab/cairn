// Generate TypeScript payload types from plugins/types/*/schema.json (JSON Schema is the
// source of truth: ADR 0006). Output: src/lib/generated/<type>.ts
import { compileFromFile } from 'json-schema-to-typescript';
import { mkdir, readdir, writeFile } from 'node:fs/promises';
import { join } from 'node:path';

const root = new URL('../../plugins/types/', import.meta.url).pathname;
const out = new URL('../src/lib/generated/', import.meta.url).pathname;
await mkdir(out, { recursive: true });

const index = [];
for (const name of (await readdir(root)).sort()) {
  const ts = await compileFromFile(join(root, name, 'schema.json'), {
    bannerComment: `// Generated from plugins/types/${name}/schema.json. Do not edit; run \`make types\`.`,
    additionalProperties: false,
  });
  await writeFile(join(out, `${name}.ts`), ts);
  index.push(`export * from './${name}';`);
  console.log(`generated ${name}.ts`);
}
await writeFile(join(out, 'index.ts'), index.join('\n') + '\n');
