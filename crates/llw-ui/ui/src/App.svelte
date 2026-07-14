<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";

  let pingReply = $state<string | null>(null);
  let pingError = $state<string | null>(null);

  async function doPing() {
    try {
      pingReply = await invoke<string>("ping");
      pingError = null;
    } catch (e) {
      pingError = String(e);
      pingReply = null;
    }
  }

  $effect(() => {
    doPing();
  });
</script>

<main>
  <h1 class="wordmark">llw</h1>
  {#if pingReply !== null}
    <p class="reply">{pingReply}</p>
  {:else if pingError !== null}
    <p class="reply error">{pingError}</p>
  {:else}
    <p class="reply muted">…</p>
  {/if}
</main>

<style>
  :global(*, *::before, *::after) {
    box-sizing: border-box;
    margin: 0;
    padding: 0;
  }

  :global(html, body) {
    width: 100%;
    height: 100%;
    background: #0b0b0f;
    color: #f2f2f5;
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial,
      sans-serif;
  }

  main {
    width: 100%;
    height: 100vh;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 0.75rem;
    background: #0b0b0f;
  }

  .wordmark {
    font-size: 2.5rem;
    font-weight: 600;
    letter-spacing: 0.08em;
    color: #f2f2f5;
  }

  .reply {
    font-size: 0.875rem;
    color: #6e6e78;
    letter-spacing: 0.04em;
  }

  .reply.error {
    color: #e57373;
  }

  .reply.muted {
    opacity: 0.5;
  }
</style>
