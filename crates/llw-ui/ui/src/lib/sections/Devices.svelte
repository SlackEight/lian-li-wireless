<script lang="ts">
  // Task 5 will fill this with bind/unbind UI.
</script>

<section class="section-content">
  <header class="section-header">
    <h1 class="section-title">Devices</h1>
    <p class="section-subtitle">Configured hardware and air-visible peripherals</p>
  </header>

  <div class="card-muted placeholder-card">
    <span>Configured devices</span>
    <span class="hint">Connects in Task 5</span>
  </div>

  <div class="card-muted placeholder-card" style="margin-top: var(--s-3);">
    <span>Air — unbound devices</span>
    <span class="hint">Connects in Task 5</span>
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
