<script lang="ts">
  // Canvas view (M3). Layout is kept per view (invariant 3). Persisting it is TODO
  // (`view.layout` via ViewLayout ops); for now positions are local to this page.
  import { Background, Controls, SvelteFlow, type Connection, type Edge as FlowEdge, type Node } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import { api } from '../api';
  import { EDGE_KINDS, type Item } from '../types';
  import ItemNode from './ItemNode.svelte';

  const nodeTypes = { item: ItemNode };
  let nodes = $state.raw<Node[]>([]);
  let edges = $state.raw<FlowEdge[]>([]);
  let edgeKind = $state<string>('relates');

  async function load() {
    const items: Item[] = await api.query({ limit: 150 });
    nodes = items.map((item, i) => ({
      id: item.id,
      type: 'item',
      position: { x: (i % 6) * 260, y: Math.floor(i / 6) * 160 },
      data: { item },
    }));
    const all = await Promise.all(items.map((i) => api.edges(i.id)));
    const seen = new Set<string>();
    edges = all.flat().filter((e) => !seen.has(e.id) && seen.add(e.id)).map((e) => ({
      id: e.id,
      source: e.from,
      target: e.to,
      label: e.kind,
    }));
  }

  async function onconnect(c: Connection) {
    const edge = await api.link(c.source, c.target, edgeKind);
    edges = [...edges, { id: edge.id, source: edge.from, target: edge.to, label: edge.kind }];
  }

  $effect(() => {
    load();
  });
</script>

<div class="toolbar">
  New links:
  <select bind:value={edgeKind}>
    {#each EDGE_KINDS as k}<option value={k}>{k}</option>{/each}
  </select>
</div>

<div class="canvas">
  <SvelteFlow bind:nodes bind:edges {nodeTypes} {onconnect} fitView>
    <Background />
    <Controls />
  </SvelteFlow>
</div>
