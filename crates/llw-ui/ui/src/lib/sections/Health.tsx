// Task 4 will fill this with live status data.
export default function Health() {
  return (
    <section className="section-content">
      <header className="section-header">
        <h1 className="section-title">Health</h1>
        <p className="section-subtitle">Link quality, dropouts, and sync status</p>
      </header>

      <div className="card-muted placeholder-card">
        <span>Waiting for daemon data</span>
        <span className="hint">Connects in Task 4</span>
      </div>

      <div className="card-muted placeholder-card" style={{ marginTop: 'var(--s-3)' }}>
        <span>Reliability &amp; dropout tracker</span>
        <span className="hint">Connects in Task 4</span>
      </div>
    </section>
  );
}
