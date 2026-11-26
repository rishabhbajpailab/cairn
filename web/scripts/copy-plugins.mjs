// Copy plugin renderers into public/ so Workers static assets serve them at
// /plugins/types/<name>/views.js (the path /api/v1/types advertises).
import { cp, mkdir, readdir, rm } from 'node:fs/promises';
import { existsSync } from 'node:fs';
import { join } from 'node:path';

const src = new URL('../../plugins/types/', import.meta.url).pathname;
const dst = new URL('../public/plugins/types/', import.meta.url).pathname;
await rm(dst, { recursive: true, force: true });
for (const name of await readdir(src)) {
  const file = join(src, name, 'views.js');
  if (!existsSync(file)) continue;
  await mkdir(join(dst, name), { recursive: true });
  await cp(file, join(dst, name, 'views.js'));
  console.log(`plugins/types/${name}/views.js`);
}
