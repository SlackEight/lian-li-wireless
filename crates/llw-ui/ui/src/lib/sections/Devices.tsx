// Task 5 will fill this with bind/unbind UI.
export default function Devices() {
  return (
    <section className="section-content">
      <header className="section-header">
        <h1 className="section-title">Devices</h1>
        <p className="section-subtitle">Configured hardware and air-visible peripherals</p>
      </header>

      <div className="card-muted placeholder-card">
        <span>Configured devices</span>
        <span className="hint">Connects in Task 5</span>
      </div>

      <div className="card-muted placeholder-card" style={{ marginTop: 'var(--s-3)' }}>
        <span>Air — unbound devices</span>
        <span className="hint">Connects in Task 5</span>
      </div>
    </section>
  );
}
