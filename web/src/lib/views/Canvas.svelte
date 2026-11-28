<script lang="ts">
  // Canvas view (M3). Layout is kept per view (invariant 4). Persisting it is TODO
  // (`view.layout` via ViewLayout ops); for now positions are local to this page.
  import { Background, Controls, SvelteFlow, type Connection, type Edge as FlowEdge, type Node } from '@xyflow/svelte';
  import '@xyflow/svelte/dist/style.css';
  import { api } from '../api';
  import { EDGE_KINDS, type Item } from '../types';
  import ItemNode from './ItemNode.svelte';

  let { ongotoinbox }: { ongotoinbox?: () => void } = $props();

  const nodeTypes = { item: ItemNode };
  let nodes = $state.raw<Node[]>([]);
  let edges = $state.raw<FlowEdge[]>([]);
  let edgeKind = $state<string>('relates');
  let loaded = $state(false);
  let error = $state('');

  async function load() {
    try {
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
      error = '';
    } catch (e) {
      error = (e as Error).message;
    } finally {
      loaded = true;
    }
  }

  async function onconnect(c: Connection) {
    try {
      const edge = await api.link(c.source, c.target, edgeKind);
      edges = [...edges, { id: edge.id, source: edge.from, target: edge.to, label: edge.kind }];
    } catch (e) {
      error = (e as Error).message;
    }
  }

  $effect(() => {
    load();
  });
</script>

<div class="toolbar">
  <label>
    New links are
    <select bind:value={edgeKind}>
      {#each EDGE_KINDS as k}<option value={k}>{k}</option>{/each}
    </select>
  </label>
  <span class="muted">Drag from the dot on a card’s right edge to another card to link them. Positions aren’t saved yet.</span>
</div>
{#if error}<p class="error" role="alert">{error}</p>{/if}

<div class="canvas">
  <SvelteFlow bind:nodes bind:edges {nodeTypes} {onconnect} fitView>
    <Background />
    <Controls />
  </SvelteFlow>
  {#if loaded && !nodes.length && !error}
    <div class="canvas-empty">
      <h2>The canvas is empty</h2>
      <p>Every active item becomes a card here, and links between them become arrows.</p>
      <p>
        <button onclick={() => ongotoinbox?.()}>Capture something in the Inbox</button>
      </p>
    </div>
  {/if}
</div>
