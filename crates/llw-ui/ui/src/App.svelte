<script lang="ts">
  import './lib/theme.css';
  import { type Section } from './lib/sections.js';
  import Sidebar from './lib/components/Sidebar.svelte';
  import DaemonBanner from './lib/components/DaemonBanner.svelte';
  import Health from './lib/sections/Health.svelte';
  import Devices from './lib/sections/Devices.svelte';
  import Lighting from './lib/sections/Lighting.svelte';
  import Cooling from './lib/sections/Cooling.svelte';

  // Active section state — Svelte 5 runes
  let active = $state<Section>('Health');

  // Daemon banner — wired to live data in Task 4
  const daemonUnreachable = false;

  function selectSection(s: Section) {
    active = s;
  }
</script>

<div class="shell">
  <Sidebar {active} onSelect={selectSection} />

  <div class="main-column">
    <DaemonBanner visible={daemonUnreachable} />

    <main class="content" class:dimmed={daemonUnreachable}>
      {#if active === 'Health'}
        <Health />
      {:else if active === 'Devices'}
        <Devices />
      {:else if active === 'Lighting'}
        <Lighting />
      {:else if active === 'Cooling'}
        <Cooling />
      {/if}
    </main>
  </div>
</div>

<style>
  .shell {
    display: flex;
    width: 100vw;
    height: 100vh;
    overflow: hidden;
    background: var(--bg);
  }

  .main-column {
    flex: 1;
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 0;
  }

  .content {
    flex: 1;
    overflow-y: auto;
    padding: var(--s-6);
    transition: opacity 200ms ease;
  }

  .content.dimmed {
    opacity: 0.5;
    pointer-events: none;
  }
</style>
