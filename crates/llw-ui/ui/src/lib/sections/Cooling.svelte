<script lang="ts">
  // M4d will implement this section.
</script>

<section class="section-content">
  <header class="section-header">
    <h1 class="section-title">Cooling</h1>
    <p class="section-subtitle">Fan curves, PWM targets, and thermal zones</p>
  </header>

  <div class="placeholder-shell">
    <!-- Dimmed curve-path silhouette -->
    <svg class="curve-svg" viewBox="0 0 200 80" fill="none" aria-hidden="true" xmlns="http://www.w3.org/2000/svg">
      <!-- axis lines -->
      <line x1="16" y1="4" x2="16" y2="68" stroke="rgba(255,255,255,0.15)" stroke-width="1"/>
      <line x1="16" y1="68" x2="196" y2="68" stroke="rgba(255,255,255,0.15)" stroke-width="1"/>
      <!-- fan curve path -->
      <path
        d="M16 64 C 50 62, 80 58, 100 45 S 150 14, 196 8"
        stroke="rgba(120,60,255,0.5)"
        stroke-width="1.5"
        stroke-linecap="round"
        fill="none"
      />
      <!-- subtle fill under curve -->
      <path
        d="M16 64 C 50 62, 80 58, 100 45 S 150 14, 196 8 L 196 68 L 16 68 Z"
        fill="rgba(80,30,200,0.07)"
      />
      <!-- control points -->
      <circle cx="16" cy="64" r="2.5" fill="rgba(150,90,255,0.4)"/>
      <circle cx="100" cy="45" r="2.5" fill="rgba(150,90,255,0.4)"/>
      <circle cx="196" cy="8" r="2.5" fill="rgba(150,90,255,0.4)"/>
      <!-- axis labels hint -->
      <text x="6" y="68" fill="rgba(255,255,255,0.15)" font-size="6" text-anchor="end">%</text>
      <text x="196" y="76" fill="rgba(255,255,255,0.15)" font-size="6" text-anchor="middle">°C</text>
    </svg>
    <p class="coming-label">Arrives in M4d</p>
    <p class="coming-hint">Custom fan curves, thermal zone targeting</p>
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

  .placeholder-shell {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: var(--s-3);
    padding: 36px var(--s-6);
    background: var(--surface);
    border: 1px solid var(--hairline);
    border-radius: var(--r-card);
    opacity: 0.45;
    filter: blur(0.4px);
    pointer-events: none;
    user-select: none;
  }

  .curve-svg {
    width: 200px;
    height: 80px;
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
