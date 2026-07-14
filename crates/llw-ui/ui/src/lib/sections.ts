/**
 * Section navigation — pure state module (no Svelte dependency).
 * Extracted so Vitest can test it without component mounting.
 */

export const SECTIONS = ['Lighting', 'Cooling', 'Devices', 'Health'] as const;
export type Section = (typeof SECTIONS)[number];

export function isValidSection(s: string): s is Section {
  return (SECTIONS as readonly string[]).includes(s);
}

export function nextSection(current: Section): Section {
  const i = SECTIONS.indexOf(current);
  return SECTIONS[(i + 1) % SECTIONS.length];
}

export function prevSection(current: Section): Section {
  const i = SECTIONS.indexOf(current);
  return SECTIONS[(i - 1 + SECTIONS.length) % SECTIONS.length];
}
