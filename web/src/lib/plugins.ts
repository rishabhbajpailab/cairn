import { api } from './api';
import type { Renderer, RendererKind, TypeViews } from './plugin-api';
import type { TypeInfo } from './types';

const cache = new Map<string, Promise<TypeViews>>();
let typeInfo: Promise<Map<string, TypeInfo>> | null = null;

export function types(): Promise<Map<string, TypeInfo>> {
  typeInfo ??= api.types().then((r) => new Map(r.types.map((t) => [t.name, t])));
  return typeInfo;
}

/** Generic fallback: pretty-printed payload. Used when a plugin omits a renderer or fails to load. */
export const jsonRenderer: Renderer = (el, item) => {
  const render = (it: typeof item) => {
    const pre = document.createElement('pre');
    pre.className = 'cairn-json';
    pre.textContent = JSON.stringify(it.payload, null, 2);
    el.replaceChildren(pre);
  };
  render(item);
  return { update: render, destroy: () => el.replaceChildren() };
};

async function load(typeName: string): Promise<TypeViews> {
  const info = (await types()).get(typeName);
  if (!info?.views) return {};
  try {
    const mod = await import(/* @vite-ignore */ info.views);
    return (mod.default ?? {}) as TypeViews;
  } catch (e) {
    console.warn(`plugin views for ${typeName} failed to load`, e);
    return {};
  }
}

export async function renderer(typeName: string, kind: RendererKind): Promise<Renderer> {
  if (!cache.has(typeName)) cache.set(typeName, load(typeName));
  const views = await cache.get(typeName)!;
  return views[kind] ?? (kind === 'node' ? views.card : undefined) ?? jsonRenderer;
}
