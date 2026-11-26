// Contract for plugins/types/*/views.js (ADR 0006). Plugins are plain ES modules.
import type { Item } from './types';

export interface RendererCtx {
  patch(partial: Record<string, unknown>): Promise<void>;
  action(name: string): Promise<void>;
  link(toId: string, kind: string): Promise<void>;
  navigate(id: string): void;
  theme: 'light' | 'dark';
}

export interface Mounted {
  update(item: Item): void;
  destroy(): void;
}

export type Renderer = (el: HTMLElement, item: Item, ctx: RendererCtx) => Mounted;

export type RendererKind = 'card' | 'node' | 'detail' | 'editor';

export type TypeViews = Partial<Record<RendererKind, Renderer>>;
