import { memo } from 'react';
import { SECTIONS, type Section } from '../sections.js';

interface Props {
  active: Section;
  onSelect: (s: Section) => void;
}

// Inline SVG icons — 16×16, 1.5px stroke, no fill
function SectionIcon({ section }: { section: Section }) {
  switch (section) {
    case 'Lighting':
      // Glow orb: circle with radiated lines
      return (
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
          <circle cx="8" cy="8" r="3" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="8" y1="1" x2="8" y2="2.5" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="8" y1="13.5" x2="8" y2="15" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="1" y1="8" x2="2.5" y2="8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="13.5" y1="8" x2="15" y2="8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="3.05" y1="3.05" x2="4.11" y2="4.11" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="11.89" y1="11.89" x2="12.95" y2="12.95" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="12.95" y1="3.05" x2="11.89" y2="4.11" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="4.11" y1="11.89" x2="3.05" y2="12.95" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
        </svg>
      );
    case 'Cooling':
      // Snowflake / fan: center + 3 axes
      return (
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
          <line x1="8" y1="1" x2="8" y2="15" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="1" y1="8" x2="15" y2="8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="3.05" y1="3.05" x2="12.95" y2="12.95" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="12.95" y1="3.05" x2="3.05" y2="12.95" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
          <line x1="5.5" y1="2.5" x2="8" y2="1" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" opacity="0.6" />
          <line x1="10.5" y1="2.5" x2="8" y2="1" stroke="currentColor" strokeWidth="1.3" strokeLinecap="round" opacity="0.6" />
        </svg>
      );
    case 'Devices':
      // Grid of 4 squares
      return (
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
          <rect x="1.5" y="1.5" width="5.5" height="5.5" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
          <rect x="9" y="1.5" width="5.5" height="5.5" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
          <rect x="1.5" y="9" width="5.5" height="5.5" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
          <rect x="9" y="9" width="5.5" height="5.5" rx="1.5" stroke="currentColor" strokeWidth="1.5" />
        </svg>
      );
    case 'Health':
      // Pulse / ECG line
      return (
        <svg width="16" height="16" viewBox="0 0 16 16" fill="none" xmlns="http://www.w3.org/2000/svg">
          <polyline
            points="1,9 4,9 5.5,4 7.5,13 9.5,7 11,9 15,9"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      );
  }
}

// memo: App re-renders on every 1s status poll; the sidebar's props (active
// section + the stable setState) only change on navigation, so this skips
// re-reconciling the four nav buttons and their inline SVGs each poll.
export default memo(function Sidebar({ active, onSelect }: Props) {
  return (
    <aside className="sidebar">
      <div className="wordmark" aria-label="llw">
        <span className="wordmark-text">llw</span>
        <span className="wordmark-dot" aria-hidden="true"></span>
      </div>

      <nav className="nav" aria-label="Main navigation">
        {SECTIONS.map((section) => (
          <button
            key={section}
            className={`nav-item${active === section ? ' active' : ''}`}
            onClick={() => onSelect(section)}
            aria-current={active === section ? 'page' : undefined}
          >
            <span className="nav-icon" aria-hidden="true">
              <SectionIcon section={section} />
            </span>
            <span className="nav-label">{section}</span>
          </button>
        ))}
      </nav>
    </aside>
  );
});
