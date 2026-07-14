<script lang="ts">
  // M4c will implement this section.
</script>

<section class="section-content">
  <header class="section-header">
    <h1 class="section-title">Lighting</h1>
    <p class="section-subtitle">RGB effects, colours, and sync profiles</p>
  </header>

  <div class="placeholder-shell">
    <!-- Dimmed fan-ring silhouette — conic gradient preview -->
    <div class="fan-ring-wrap" aria-hidden="true">
      <div class="fan-ring outer">
        <div class="fan-ring inner"></div>
      </div>
      <div class="fan-ring-glow"></div>
    </div>
    <p class="coming-label">Arrives in M4c</p>
    <p class="coming-hint">Effect editor, per-device colour and sync profiles</p>
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

  /* Placeholder silhouette */
  .placeholder-shell {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--s-3);
    padding: 48px var(--s-6);
    background: var(--surface);
    border: 1px solid var(--hairline);
    border-radius: var(--r-card);
    opacity: 0.45;
    filter: blur(0.4px);
    pointer-events: none;
    user-select: none;
  }

  .fan-ring-wrap {
    position: relative;
    width: 96px;
    height: 96px;
  }

  .fan-ring {
    position: absolute;
    inset: 0;
    border-radius: 50%;
  }

  .fan-ring.outer {
    background: conic-gradient(
      from 200deg,
      #0000ff 0%,
      #8800ff 30%,
      #141419 55%,
      #0000ff 100%
    );
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .fan-ring.inner {
    position: relative;
    inset: auto;
    width: 62px;
    height: 62px;
    background: var(--surface);
    border-radius: 50%;
  }

  .fan-ring-glow {
    position: absolute;
    inset: -8px;
    border-radius: 50%;
    background: radial-gradient(
      ellipse at center,
      rgba(100, 30, 255, 0.3) 0%,
      transparent 70%
    );
  }

  .coming-label {
    font-size: 12px;
    font-weight: 600;
    color: var(--text-dim);
    letter-spacing: 0.04em;
    text-transform: uppercase;
  }

  .coming-hint {
    font-size: 12px;
    color: var(--text-muted);
    text-align: center;
    max-width: 220px;
  }
</style>
