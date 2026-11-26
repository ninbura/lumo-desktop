import { logError } from '../shared/logger';
import { tauriInvoke } from '../shared/tauri';

// Both are static in login.html, so absence is a broken build rather than a runtime case to handle.
const button = document.getElementById('sign-in')!;
const hint = document.getElementById('hint')!;

const HINTS = {
    idle: '',
    waiting: 'Finish signing in, in your browser. This window continues automatically.',
    failed: 'Something went wrong starting sign-in. Please try again.',
};

/* The shell navigates this window away once the fork is handed over, so there is no success
 * state to render here. Requesting sign-in twice is meaningful rather than a mistake: the
 * second request completes a sign-in the browser could not report back, which is why the
 * button stays enabled. The first request on a fresh page always starts a new sign-in, so a
 * failed redemption that reloads this page leaves the pending one to the browser callback. */
let started = false;

const setState = (state: keyof typeof HINTS) => {
    hint.textContent = HINTS[state];
    hint.className = `login-hint ${state === 'idle' ? '' : state}`.trim();
};

button.addEventListener('click', async () => {
    try {
        await tauriInvoke('start_browser_login', { redeem: started });
        started = true;
        button.textContent = "I've signed in";
        setState('waiting');
    } catch (e) {
        logError('sign-in request failed', e);
        setState('failed');
        if (!started) {
            button.textContent = 'Sign in with your browser';
        }
    }
});
