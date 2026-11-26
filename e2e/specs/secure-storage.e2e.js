const { waitForStubReady } = require('../support/waits');

// The Linux container deliberately has no Secret Service. Native macOS/Windows
// runs expect an available store; Linux desktop runners can set the expectation.
const expected = process.env.LUMO_E2E_EXPECT_SECURE_STORAGE || 'unavailable';
if (!['available', 'unavailable'].includes(expected)) {
    throw new Error('LUMO_E2E_EXPECT_SECURE_STORAGE must be available or unavailable');
}
describe('secure storage detection', () => {
    it(`reports storage ${expected} for this runner`, async () => {
        await waitForStubReady();

        await browser.execute(() => {
            window.__secureStorageVerdict = 'pending';
            window.__TAURI_INTERNALS__
                .invoke('is_secure_storage_available')
                .then((available) => {
                    window.__secureStorageVerdict = available ? 'available' : 'unavailable';
                })
                .catch((e) => {
                    window.__secureStorageVerdict = `error: ${e}`;
                });
        });

        await browser.waitUntil(
            async () => (await browser.execute(() => window.__secureStorageVerdict)) !== 'pending',
            { timeout: 20_000, timeoutMsg: 'the storage probe never returned' }
        );

        const verdict = await browser.execute(() => window.__secureStorageVerdict);
        expect(verdict).toBe(expected);
    });
});
