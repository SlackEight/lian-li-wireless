// M4c will implement this section.
export default function Lighting() {
  return (
    <section className="section-content">
      <header className="section-header">
        <h1 className="section-title">Lighting</h1>
        <p className="section-subtitle">RGB effects, colours, and sync profiles</p>
      </header>

      <div className="placeholder-shell">
        {/* Dimmed fan-ring silhouette — conic gradient preview */}
        <div className="fan-ring-wrap" aria-hidden="true">
          <div className="fan-ring outer">
            <div className="fan-ring inner"></div>
          </div>
          <div className="fan-ring-glow"></div>
        </div>
        <p className="coming-label">Arrives in M4c</p>
        <p className="coming-hint">Effect editor, per-device colour and sync profiles</p>
      </div>
    </section>
  );
}
