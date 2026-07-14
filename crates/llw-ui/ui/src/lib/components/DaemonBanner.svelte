<script lang="ts">
  interface Props {
    visible: boolean;
    message?: string;
  }

  let { visible, message = 'daemon unreachable — retrying' }: Props = $props();
</script>

{#if visible}
  <div class="daemon-banner" role="status" aria-live="polite">
    <span class="banner-dot" aria-hidden="true"></span>
    <span class="banner-text">{message}</span>
  </div>
{/if}

<style>
  .daemon-banner {
    display: flex;
    align-items: center;
    gap: var(--s-2);
    padding: var(--s-2) var(--s-6);
    background: rgba(232, 168, 75, 0.1);
    border-bottom: 1px solid rgba(232, 168, 75, 0.2);
    font-size: 12px;
    color: var(--warn);
    animation: slide-down 200ms ease;
  }

  @keyframes slide-down {
    from { transform: translateY(-100%); opacity: 0; }
    to   { transform: translateY(0);    opacity: 1; }
  }

  .banner-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--warn);
    animation: pulse 1.8s ease-in-out infinite;
    flex-shrink: 0;
  }

  @keyframes pulse {
    0%, 100% { opacity: 1; }
    50%       { opacity: 0.35; }
  }

  .banner-text {
    font-family: var(--font);
    letter-spacing: 0.01em;
  }
</style>
