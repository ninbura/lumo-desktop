/**
 * Lumo desktop: the quick-ask window.
 *
 * Takes a question and hands it to the main window via `inject_search`, then hides itself.
 * The shell keeps the question and the web app redeems it through `bridges/quick-ask.ts`.
 */
import { logError } from '../shared/logger';
import { tauriInvoke } from '../shared/tauri';

document.addEventListener('DOMContentLoaded', () => {
  const searchInput = document.getElementById('search') as HTMLInputElement;
  const hint = document.getElementById('hint') as HTMLElement;
  const pill = document.getElementById('pill') as HTMLElement;

  const showError = (message: string) => {
    hint.textContent = message;
    hint.hidden = false;
  };

  const clearError = () => {
    hint.textContent = '';
    hint.hidden = true;
  };

  /*
   * The window is only hidden between uses, and the OS might show the last frame it drew as soon as
   * it is shown again, resulting in a flicker. So the pill is put away (invisible, field emptied)
   * and that state is painted *before* the window hides; the next show then starts from an empty 
   * frame and fades in, instead of flashing the old pill and restarting the animation over it.
   */
  let hiding = false;

  const nextFrame = () =>
    new Promise<void>((resolve) => {
      requestAnimationFrame(() => resolve());
      // rAF can stall if the window is already occluded; never let that block hiding.
      setTimeout(resolve, 50);
    });

  const hide = async () => {
    if (hiding) return;
    hiding = true;
    pill.classList.remove('appearing');
    pill.classList.add('stowed');
    searchInput.value = '';
    clearError();
    try {
      // One frame to apply the change, one to be sure it has reached the screen.
      await nextFrame();
      await nextFrame();
      // Shown again meanwhile (Ctrl+L right after closing): stay open.
      if (!pill.classList.contains('stowed')) return;
      await tauriInvoke('hide_search');
    } catch (error) {
      logError('could not hide search:', error);
      reveal();
    } finally {
      hiding = false;
    }
  };

  /** Fade the pill in, once per show: only from the put-away state. */
  const reveal = () => {
    searchInput.focus();
    if (!pill.classList.contains('stowed')) return;
    clearError();
    pill.classList.remove('stowed');
    pill.classList.add('appearing');
  };

  reveal();

  // The shell calls this on every show. `focus` alone is not enough: on Windows the native window
  // can be focused without WebView2 getting focus, so the page never hears about it and the pill
  // would stay stowed (an invisible window holding focus until clicked).
  window.__lumoRevealSearch = reveal;
  window.addEventListener('focus', reveal);
  document.addEventListener('visibilitychange', () => {
    if (document.visibilityState === 'visible') reveal();
  });

  searchInput.addEventListener('input', clearError);

  const ask = async () => {
    const query = searchInput.value.trim();
    if (!query) {
      return;
    }
    try {
      await tauriInvoke('inject_search', { query });
    } catch (error) {
      // Left open on failure, so the question is not lost to a window that vanishes.
      showError(String(error));
      return;
    }
    await hide();
  };

  const askButton = document.getElementById('ask') as HTMLButtonElement;
  // Keep the caret in the field: clicking the button must not take focus from it.
  askButton.addEventListener('mousedown', (e) => e.preventDefault());
  askButton.addEventListener('click', () => {
    searchInput.focus();
    void ask();
  });

  searchInput.addEventListener('keydown', async (e) => {
    if (e.key === 'Enter') {
      await ask();
    } else if (e.key === 'Escape') {
      await hide();
    }
  });

  window.addEventListener('blur', () => void hide());
});
