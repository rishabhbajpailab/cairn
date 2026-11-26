<script lang="ts">
  import { api } from '../api';
  import { subscribe } from '../live';
  import type { Item } from '../types';
  import ItemView from './ItemView.svelte';

  let items = $state<Item[]>([]);
  let text = $state('');
  let error = $state('');

  async function refresh() {
    try {
      items = await api.query({ updated_since: '-7d', limit: 200 });
      error = '';
    } catch (e) {
      error = (e as Error).message;
    }
  }

  async function capture(e: SubmitEvent) {
    e.preventDefault();
    if (!text.trim()) return;
    await api.capture(text);
    text = '';
    // The item appears when the queue consumer writes it; the Hub event triggers refresh.
  }

  function replace(next: Item) {
    items = items.map((i) => (i.id === next.id ? next : i));
  }

  $effect(() => {
    refresh();
    // Captures land asynchronously (queue consumer), so refresh when the Hub says so.
    return subscribe(() => refresh());
  });
</script>

<form class="capture" onsubmit={capture}>
  <input bind:value={text} placeholder="Capture… (todo: / htt: / remind: prefixes work)" />
  <button type="submit">Add</button>
</form>

{#if error}<p class="error">{error}</p>{/if}

<ul class="list">
  {#each items as item (item.id)}
    <li>
      <span class="badge">{item.type}</span>
      <ItemView {item} onchange={replace} />
      {#each item.tags as tag}<span class="tag">#{tag}</span>{/each}
    </li>
  {/each}
</ul>
