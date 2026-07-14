<script lang="ts">
  import { SECTIONS, type Section } from '../sections.js';

  interface Props {
    active: Section;
    onSelect: (s: Section) => void;
  }

  let { active, onSelect }: Props = $props();
</script>

<aside class="sidebar">
  <!-- Wordmark -->
  <div class="wordmark" aria-label="llw">
    <span class="wordmark-text">llw</span><span class="wordmark-dot" aria-hidden="true"></span>
  </div>

  <!-- Nav items -->
  <nav class="nav" aria-label="Main navigation">
    {#each SECTIONS as section}
      <button
        class="nav-item"
        class:active={active === section}
        onclick={() => onSelect(section)}
        aria-current={active === section ? 'page' : undefined}
      >
        <!-- Inline SVG icons — 16×16, 1.5px stroke, no fill -->
        <span class="nav-icon" aria-hidden="true">
          {#if section === 'Lighting'}
            <!-- Glow orb: circle with radiated lines -->
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <circle cx="8" cy="8" r="3" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="8" y1="1" x2="8" y2="2.5" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="8" y1="13.5" x2="8" y2="15" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="1" y1="8" x2="2.5" y2="8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="13.5" y1="8" x2="15" y2="8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="3.05" y1="3.05" x2="4.11" y2="4.11" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="11.89" y1="11.89" x2="12.95" y2="12.95" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="12.95" y1="3.05" x2="11.89" y2="4.11" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="4.11" y1="11.89" x2="3.05" y2="12.95" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
            </svg>
          {:else if section === 'Cooling'}
            <!-- Snowflake / fan: center + 3 axes -->
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <line x1="8" y1="1" x2="8" y2="15" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="1" y1="8" x2="15" y2="8" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="3.05" y1="3.05" x2="12.95" y2="12.95" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <line x1="12.95" y1="3.05" x2="3.05" y2="12.95" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"/>
              <!-- small ticks on axes -->
              <line x1="5.5" y1="2.5" x2="8" y2="1" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" opacity="0.6"/>
              <line x1="10.5" y1="2.5" x2="8" y2="1" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" opacity="0.6"/>
            </svg>
          {:else if section === 'Devices'}
            <!-- Grid of 4 squares -->
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <rect x="1.5" y="1.5" width="5.5" height="5.5" rx="1.5" stroke="currentColor" stroke-width="1.5"/>
              <rect x="9" y="1.5" width="5.5" height="5.5" rx="1.5" stroke="currentColor" stroke-width="1.5"/>
              <rect x="1.5" y="9" width="5.5" height="5.5" rx="1.5" stroke="currentColor" stroke-width="1.5"/>
              <rect x="9" y="9" width="5.5" height="5.5" rx="1.5" stroke="currentColor" stroke-width="1.5"/>
            </svg>
          {:else}
            <!-- Health: pulse / ECG line -->
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
              <polyline
                points="1,9 4,9 5.5,4 7.5,13 9.5,7 11,9 15,9"
                stroke="currentColor"
                stroke-width="1.5"
                stroke-linecap="round"
                stroke-linejoin="round"
              />
            </svg>
          {/if}
        </span>
        <span class="nav-label">{section}</span>
      </button>
    {/each}
  </nav>
</aside>

<style>
  .sidebar {
    width: var(--sidebar-width);
    min-width: var(--sidebar-width);
    height: 100%;
    background: var(--surface);
    border-right: 1px solid var(--hairline);
    display: flex;
    flex-direction: column;
    gap: 0;
    padding: var(--s-6) var(--s-3);
    flex-shrink: 0;
  }

  /* Wordmark */
  .wordmark {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    padding: 0 var(--s-2);
    margin-bottom: var(--s-6);
    user-select: none;
  }

  .wordmark-text {
    font-size: 16px;
    font-weight: 600;
    color: var(--text);
    letter-spacing: 0.04em;
  }

  /* The one charged accent: a tiny purple glow dot */
  .wordmark-dot {
    display: inline-block;
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: rgba(150, 90, 255, 0.9);
    box-shadow:
      0 0 6px rgba(120, 60, 255, 0.8),
      0 0 14px rgba(80, 20, 220, 0.4);
    margin-bottom: 1px; /* sits slightly above baseline */
  }

  /* Nav */
  .nav {
    display: flex;
    flex-direction: column;
    gap: var(--s-1);
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: var(--s-3);
    padding: var(--s-2) var(--s-3);
    border-radius: var(--r-control);
    border: none;
    background: transparent;
    color: var(--text-dim);
    font-size: 13px;
    font-family: var(--font);
    cursor: pointer;
    text-align: left;
    transition:
      background 120ms ease,
      color 120ms ease;
    width: 100%;
  }

  .nav-item:hover {
    background: var(--surface-2);
    color: var(--text);
  }

  .nav-item.active {
    background: var(--accent);
    color: var(--accent-text);
    font-weight: 500;
  }

  .nav-icon {
    display: flex;
    align-items: center;
    justify-content: center;
    flex-shrink: 0;
    width: 16px;
    height: 16px;
  }

  .nav-label {
    flex: 1;
  }
</style>
