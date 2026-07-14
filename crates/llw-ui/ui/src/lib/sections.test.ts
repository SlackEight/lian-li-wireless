import { describe, it, expect } from 'vitest';
import { SECTIONS, isValidSection, nextSection, prevSection, type Section } from './sections.js';

describe('sections module', () => {
  it('exports the four expected sections in order', () => {
    expect(SECTIONS).toEqual(['Lighting', 'Cooling', 'Devices', 'Health']);
  });

  it('isValidSection returns true for valid section names', () => {
    for (const s of SECTIONS) {
      expect(isValidSection(s)).toBe(true);
    }
  });

  it('isValidSection returns false for invalid names', () => {
    expect(isValidSection('')).toBe(false);
    expect(isValidSection('lighting')).toBe(false); // case-sensitive
    expect(isValidSection('Dashboard')).toBe(false);
  });

  it('nextSection cycles forward through sections', () => {
    expect(nextSection('Lighting')).toBe('Cooling');
    expect(nextSection('Cooling')).toBe('Devices');
    expect(nextSection('Devices')).toBe('Health');
    expect(nextSection('Health')).toBe('Lighting'); // wraps around
  });

  it('prevSection cycles backward through sections', () => {
    expect(prevSection('Cooling')).toBe('Lighting');
    expect(prevSection('Devices')).toBe('Cooling');
    expect(prevSection('Health')).toBe('Devices');
    expect(prevSection('Lighting')).toBe('Health'); // wraps around
  });

  it('simulates section switching state logic', () => {
    // Simulates what App does: switching active section
    let active: Section = 'Health';

    function select(s: Section) {
      active = s;
    }

    select('Lighting');
    expect(active).toBe('Lighting');

    select('Devices');
    expect(active).toBe('Devices');

    // Clicking the same section stays on it
    select('Devices');
    expect(active).toBe('Devices');

    // Cycling through all sections
    for (let i = 0; i < SECTIONS.length; i++) {
      const expected = SECTIONS[(SECTIONS.indexOf(active) + 1) % SECTIONS.length];
      active = nextSection(active);
      expect(active).toBe(expected);
    }
    // Should have cycled back to original
    expect(active).toBe('Devices');
  });
});
