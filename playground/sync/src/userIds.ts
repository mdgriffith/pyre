export const SEEDED_USER_COUNT = 20

export function userIdForIndex(index: number): string {
  return `01890f47-2f00-7000-8000-${index.toString(16).padStart(12, '0')}`
}
