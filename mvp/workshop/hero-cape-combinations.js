// Original combinations of the owner's shortlisted C2, D1 and A2, 2026-09-14.
// Exact C2/A2 paths are reused; all exported SVG strings are self-contained.
// F2 is the owner's color trial in the sidebar; the other two remain studies.
// --mark-paper must match the surface beneath the mark.
import { concepts as creatureConcepts } from './hero-beast-motion.js';
import { concepts as intelligenceConcepts } from './hero-mind-power.js';

const innerSvg = (svg) => svg.replace(/^\s*<svg\b[^>]*>/, '').replace(/<\/svg>\s*$/, '').trim();
const titan = innerSvg(creatureConcepts.find(({ id }) => id === 'beast-2').svg);
const eye = innerSvg(intelligenceConcepts.find(({ id }) => id === 'mind-2').svg);
const paperBadge = (markup) => markup
  .replaceAll('var(--mark-accent, currentColor)', 'var(--mark-paper, white)')
  .replaceAll('fill="currentColor"', 'fill="var(--mark-paper, white)"');

// An open turned collar, widening drape and uneven wind-lifted hem give the
// two heraldic concepts the same garment, keeping the emblem as the variable.
const heraldicCape = `
  <path fill="currentColor" d="M29 18Q38 24 47 18Q46 35 59 49Q48 48 42 58Q28 50 5 55Q16 41 19 27Q22 20 29 18Z"/>
  <path fill="var(--mark-accent, currentColor)" d="M29 7Q38 13 49 7L47 14Q38 20 30 14Z"/>
  <path d="M22 26Q20 34 15 41" stroke="var(--mark-paper, white)" stroke-width="2.5" stroke-linecap="round"/>
`;

// Only the source's exterior contour is needed for the paper underlay: its
// face counters remain the unmodified source's transparent negative shapes.
const titanOuter = titan.match(/\bd="([^"]+)"/)[1].split('ZM16')[0] + 'Z';
const titanFlame = titan.match(/<path\b[^>]*\bd="([^"]+)"[^>]*\/>\s*$/)[1];

export const concepts = [
  {
    id: 'cape-1',
    group: 'cape',
    name: 'Титан на плаще',
    idea: 'Плащ носит знак карманного титана: маленький союзник становится эмблемой большой силы.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      ${heraldicCape}
      <g transform="translate(20 24) scale(.43)">${paperBadge(titan)}</g>
    </svg>`,
  },
  {
    id: 'cape-2',
    group: 'cape',
    name: 'Титан в плаще',
    idea: 'Дружелюбный титан в плаще: спокойное лицо, большая сила и маленький огонь на кончике хвоста.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      <path fill="var(--mark-accent, currentColor)" d="M31 26Q19 23 5 31L10 40 3 56Q15 50 25 58L35 43Z"/>
      <g transform="translate(11 7) scale(.8)">
        <path d="${titanOuter}" fill="var(--mark-paper, white)" stroke="var(--mark-paper, white)" stroke-width="5" stroke-linejoin="round"/>
        <path d="${titanFlame}" fill="var(--mark-paper, white)" stroke="var(--mark-paper, white)" stroke-width="4" stroke-linejoin="round"/>
        ${titan}
      </g>
    </svg>`,
  },
  {
    id: 'cape-3',
    group: 'cape',
    name: 'Око на плаще',
    idea: 'Развевающийся плащ несёт ясное око: герой видит главное и берёт сложность на себя.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      ${heraldicCape}
      <g transform="translate(18 24) scale(.49)">${paperBadge(eye)}</g>
    </svg>`,
  },
];
