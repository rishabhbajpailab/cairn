<script lang="ts">
  import { getToken, setToken } from './lib/api';
  import Canvas from './lib/views/Canvas.svelte';
  import Inbox from './lib/views/Inbox.svelte';

  let view = $state<'inbox' | 'canvas'>('inbox');
  let hasToken = $state(!!getToken());
  let tokenInput = $state('');
</script>

<header>
  <strong>cairn</strong>
  <nav>
    <button class:active={view === 'inbox'} onclick={() => (view = 'inbox')}>Inbox</button>
    <button class:active={view === 'canvas'} onclick={() => (view = 'canvas')}>Canvas</button>
  </nav>
</header>

<main>
  {#if !hasToken}
    <form
      class="capture"
      onsubmit={(e) => {
        e.preventDefault();
        setToken(tokenInput);
        hasToken = true;
      }}
    >
      <input type="password" bind:value={tokenInput} placeholder="API token (write scope)" />
      <button type="submit">Save</button>
    </form>
  {:else if view === 'inbox'}
    <Inbox />
  {:else}
    <Canvas />
  {/if}
</main>
