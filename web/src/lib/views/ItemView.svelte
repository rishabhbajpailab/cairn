<script lang="ts">
  // Mounts a plugin renderer (card | node | detail | editor) into a plain DOM element.
  import { api } from '../api';
  import type { Mounted, RendererKind } from '../plugin-api';
  import { renderer } from '../plugins';
  import type { Item } from '../types';

  let { item, kind = 'card', onchange }: { item: Item; kind?: RendererKind; onchange?: (i: Item) => void } = $props();

  let host: HTMLDivElement;
  let mounted: Mounted | null = null;
  let mountedType = '';

  const ctx = {
    patch: async (partial: Record<string, unknown>) => {
      const next = await api.patch(item.id, partial);
      onchange?.(next);
    },
    action: async (name: string) => console.info('TODO(M2): run action', name),
    link: async (to: string, kind: string) => void (await api.link(item.id, to, kind)),
    navigate: (id: string) => (location.hash = `#/item/${id}`),
    theme: (matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light') as 'light' | 'dark',
  };

  $effect(() => {
    const current = item;
    if (mounted && mountedType === current.type) {
      mounted.update(current);
      return;
    }
    let cancelled = false;
    renderer(current.type, kind).then((r) => {
      if (cancelled) return;
      mounted?.destroy();
      mounted = r(host, current, ctx);
      mountedType = current.type;
    });
    return () => {
      cancelled = true;
    };
  });

  $effect(() => () => mounted?.destroy());
</script>

<div class="item" data-type={item.type} bind:this={host}></div>
