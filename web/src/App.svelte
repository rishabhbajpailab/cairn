<script lang="ts">
  import { ApiError, api, clearToken, getToken, setToken } from './lib/api';
  import Canvas from './lib/views/Canvas.svelte';
  import Inbox from './lib/views/Inbox.svelte';

  let view = $state<'inbox' | 'canvas'>('inbox');
  let hasToken = $state(!!getToken());
  let tokenInput = $state('');
  let tokenError = $state('');
  let checking = $state(false);

  async function saveToken(e: SubmitEvent) {
    e.preventDefault();
    const t = tokenInput.trim();
    if (!t) return;
    checking = true;
    tokenError = '';
    setToken(t);
    try {
      await api.types(); // cheapest authenticated read: proves the token works
      hasToken = true;
      tokenInput = '';
    } catch (err) {
      clearToken();
      const status = err instanceof ApiError ? err.status : 0;
      tokenError =
        status === 401
          ? 'Cairn doesn’t recognise that token. Check its sha256 is in CAIRN_TOKENS and the container was recreated.'
          : status === 403
            ? 'That token can’t read items (a capture-only token?). The web app needs a token with write scope.'
            : `Couldn’t reach Cairn: ${(err as Error).message}`;
    } finally {
      checking = false;
    }
  }

  function signOut() {
    clearToken();
    hasToken = false;
  }
</script>

<header>
  <strong>cairn</strong>
  {#if hasToken}
    <nav>
      <button class:active={view === 'inbox'} onclick={() => (view = 'inbox')}>Inbox</button>
      <button class:active={view === 'canvas'} onclick={() => (view = 'canvas')}>Canvas</button>
    </nav>
    <button class="link push" onclick={signOut} title="Forget the API token stored in this browser">Forget token</button>
  {/if}
</header>

<main>
  {#if !hasToken}
    <section class="welcome">
      <h1>Welcome to Cairn</h1>
      <p>
        Cairn is your second brain: capture thoughts, to-dos and reminders from here, your phone, webhooks or Discord, then
        arrange them on a canvas. First, connect this browser with an API token.
      </p>
      <form class="capture" onsubmit={saveToken}>
        <input
          type="password"
          bind:value={tokenInput}
          placeholder="Paste your API token (write scope)"
          autocomplete="current-password"
          aria-label="API token"
        />
        <button type="submit" disabled={checking || !tokenInput.trim()}>{checking ? 'Checking…' : 'Connect'}</button>
      </form>
      {#if tokenError}<p class="error" role="alert">{tokenError}</p>{/if}
      <details>
        <summary>Don’t have a token yet?</summary>
        <ol>
          <li>Pick a long random string, for example <code>openssl rand -hex 32</code>. That’s your token; keep it secret.</li>
          <li>Run <code>make hash</code> in the Cairn checkout to get its sha256.</li>
          <li>
            Add <code>ui:write:&lt;sha256&gt;</code> to <code>CAIRN_TOKENS</code> in <code>deploy/cairn.env</code> (separate
            entries with <code>;</code>), then recreate the <code>cairn</code> container.
          </li>
          <li>Paste the token (not the hash) above. It’s kept in this browser only.</li>
        </ol>
        <p class="muted">More in <code>docs/ingest.md</code>.</p>
      </details>
    </section>
  {:else if view === 'inbox'}
    <Inbox ongotocanvas={() => (view = 'canvas')} />
  {:else}
    <Canvas ongotoinbox={() => (view = 'inbox')} />
  {/if}
</main>
