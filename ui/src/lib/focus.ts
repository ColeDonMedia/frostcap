/**
 * Focus trap decision for Tab inside a modal.
 * @param count  number of focusable elements in the dialog
 * @param active index of the focused one, or -1 if focus is outside / on a non-focusable node
 * @returns index to focus, -1 to focus the dialog itself, or null to let the browser handle Tab
 */
export function trapTab(count: number, active: number, shift: boolean): number | null {
  if (count === 0) return -1;
  if (active < 0) return shift ? count - 1 : 0;
  if (shift && active === 0) return count - 1;
  if (!shift && active === count - 1) return 0;
  return null;
}
