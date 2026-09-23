// Original, code-authored CommunityHero emblem studies, 2026-09-14.
// Candidate assets for the comparison sheet; no selected or adopted identity.
// Every shape is authored here, with no external assets or borrowed lettering.
// Keep the whole 64 × 64 viewBox; consumers supply accessible labels and color.
export const concepts = [
  {
    id: 'mind-1',
    group: 'mind',
    name: 'Дальний взгляд',
    idea: 'Спокойный визор героя замечает главное раньше, чем оно становится проблемой.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      <path d="M12 27C12 15 21 8 32 8C43 8 52 15 52 27H42L37 22H27L22 27H12Z" fill="currentColor"/>
      <path d="M11 32H23L28 27H36L41 32H53L48 41H38L32 37L26 41H16L11 32Z" fill="var(--mark-accent, currentColor)"/>
      <path d="M18 46H27L32 43L37 46H46L42 52C39 56 25 56 22 52L18 46Z" fill="currentColor"/>
    </svg>`,
  },
  {
    id: 'mind-2',
    group: 'mind',
    name: 'Ясное око',
    idea: 'Широкий внимательный взгляд удерживает целое и точно выделяет суть.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      <path d="M5 30C12 17 21 11 32 11C43 11 52 17 59 30L51 33C45 23 39 19 32 19C25 19 19 23 13 33L5 30Z" fill="currentColor"/>
      <path d="M9 39L17 35C21 42 26 46 32 46C38 46 43 42 47 35L55 39C49 49 42 54 32 54C22 54 15 49 9 39Z" fill="currentColor"/>
      <path d="M33 25C38 25 42 29 42 34C42 39 38 43 33 43C28 43 24 39 24 34V29H29V25H33Z" fill="var(--mark-accent, currentColor)"/>
    </svg>`,
  },
  {
    id: 'mind-3',
    group: 'mind',
    name: 'Мудрый спутник',
    idea: 'Собранная сова соединяет проницательность, присутствие и дружелюбную готовность помочь.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      <path fill-rule="evenodd" d="M10 11L22 16C28 13 36 13 42 16L54 11V34C54 47 44 55 32 57C20 55 10 47 10 34V11ZM17 27C17 23 20 20 24 20C28 20 29 23 29 27V34H24C20 34 17 31 17 27ZM47 27C47 23 44 20 40 20C36 20 35 23 35 27V34H40C44 34 47 31 47 27ZM21 38H43L32 54L21 38Z" fill="currentColor"/>
      <path d="M27 41H37L32 48L27 41Z" fill="var(--mark-accent, currentColor)"/>
    </svg>`,
  },
  {
    id: 'power-1',
    group: 'power',
    name: 'Тихий реактор',
    idea: 'Массивные дуги удерживают живое ядро: огромная энергия находится под контролем.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      <path d="M26 7V16C19 18 14 24 14 32C14 40 19 46 26 48V57C14 54 5 44 5 32C5 20 14 10 26 7Z" fill="currentColor"/>
      <path d="M38 7C50 10 59 20 59 32C59 44 50 54 38 57V48C45 46 50 40 50 32C50 24 45 18 38 16V7Z" fill="currentColor"/>
      <path d="M32 18C38 23 42 27 42 32C42 38 38 42 32 46C26 42 22 38 22 32C22 27 26 23 32 18Z" fill="var(--mark-accent, currentColor)"/>
    </svg>`,
  },
  {
    id: 'power-2',
    group: 'power',
    name: 'Сила в резерве',
    idea: 'Плотный асимметричный корпус хранит направленный заряд, готовый включиться в нужный момент.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      <path fill-rule="evenodd" d="M8 23L29 8H45L56 24V43L35 56H20L8 44V23ZM19 27L32 18H40L46 27V38L33 47H25L19 39V27Z" fill="currentColor"/>
      <path d="M25 33L35 25C36 24 38 25 39 27L40 30L30 39C29 40 27 39 26 37L25 33Z" fill="var(--mark-accent, currentColor)"/>
    </svg>`,
  },
  {
    id: 'power-3',
    group: 'power',
    name: 'Пробуждение',
    idea: 'Разомкнутая оболочка раскрывает внутреннюю мощь, когда человеку нужна помощь.',
    svg: `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 64 64" fill="none">
      <path d="M9 15C7 29 10 45 29 57V45C22 38 18 32 18 26L23 19L17 8L9 15Z" fill="currentColor"/>
      <path d="M55 15C57 29 54 45 35 57V45C42 38 46 32 46 26L41 19L47 8L55 15Z" fill="currentColor"/>
      <path d="M32 7C36 13 39 20 38 26C37 31 35 34 32 38C29 34 27 31 26 26C25 20 28 13 32 7Z" fill="var(--mark-accent, currentColor)"/>
    </svg>`,
  },
];
