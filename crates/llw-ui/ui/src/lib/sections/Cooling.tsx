// M4d will implement this section.
export default function Cooling() {
  return (
    <section className="section-content">
      <header className="section-header">
        <h1 className="section-title">Cooling</h1>
        <p className="section-subtitle">Fan curves, PWM targets, and thermal zones</p>
      </header>

      <div className="placeholder-shell">
        {/* Dimmed curve-path silhouette */}
        <svg className="curve-svg" viewBox="0 0 200 80" fill="none" aria-hidden="true" xmlns="http://www.w3.org/2000/svg">
          <line x1="16" y1="4" x2="16" y2="68" stroke="rgba(255,255,255,0.15)" strokeWidth="1" />
          <line x1="16" y1="68" x2="196" y2="68" stroke="rgba(255,255,255,0.15)" strokeWidth="1" />
          <path
            d="M16 64 C 50 62, 80 58, 100 45 S 150 14, 196 8"
            stroke="rgba(120,60,255,0.5)"
            strokeWidth="1.5"
            strokeLinecap="round"
            fill="none"
          />
          <path
            d="M16 64 C 50 62, 80 58, 100 45 S 150 14, 196 8 L 196 68 L 16 68 Z"
            fill="rgba(80,30,200,0.07)"
          />
          <circle cx="16" cy="64" r="2.5" fill="rgba(150,90,255,0.4)" />
          <circle cx="100" cy="45" r="2.5" fill="rgba(150,90,255,0.4)" />
          <circle cx="196" cy="8" r="2.5" fill="rgba(150,90,255,0.4)" />
          <text x="6" y="68" fill="rgba(255,255,255,0.15)" fontSize="6" textAnchor="end">%</text>
          <text x="196" y="76" fill="rgba(255,255,255,0.15)" fontSize="6" textAnchor="middle">°C</text>
        </svg>
        <p className="coming-label">Arrives in M4d</p>
        <p className="coming-hint">Custom fan curves, thermal zone targeting</p>
      </div>
    </section>
  );
}
