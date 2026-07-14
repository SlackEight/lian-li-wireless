<script lang="ts">
  // Task 4 will fill this with live status data.
</script>

<section class="section-content">
  <header class="section-header">
    <h1 class="section-title">Health</h1>
    <p class="section-subtitle">Link quality, dropouts, and sync status</p>
  </header>

  <div class="card-muted placeholder-card">
    <span>Waiting for daemon data</span>
    <span class="hint">Connects in Task 4</span>
  </div>

  <div class="card-muted placeholder-card" style="margin-top: var(--s-3);">
    <span>Reliability &amp; dropout tracker</span>
    <span class="hint">Connects in Task 4</span>
  </div>
</section>

<style>
  .section-content {
    display: flex;
    flex-direction: column;
    gap: var(--s-4);
  }

  .section-header {
    margin-bottom: var(--s-2);
  }

  .section-title {
    font-size: 18px;
    font-weight: 600;
    color: var(--text);
    letter-spacing: -0.01em;
  }

  .section-subtitle {
    font-size: 13px;
    color: var(--text-muted);
    margin-top: var(--s-1);
  }

  .placeholder-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .hint {
    font-size: 11px;
    color: var(--text-muted);
    opacity: 0.6;
  }
</style>
