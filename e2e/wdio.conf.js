const path = require('node:path');

for (const key of ['LUMO_E2E_CONFIG_DIR', 'LUMO_E2E_SCRATCH', 'LUMO_E2E_ESCAPE']) {
    if (!process.env[key] || !path.isAbsolute(process.env[key])) {
        throw new Error(`${key} must be absolute; run node e2e/run-native.mjs`);
    }
}

exports.config = {
    runner: 'local',
    specs: ['./specs/**/*.e2e.js'],
    maxInstances: 1,
    services: [['@wdio/tauri-service', {
        driverProvider: 'embedded',
        appBinaryPath: process.env.LUMO_APP_BINARY,
        windowLabel: 'lumo-window',
        embeddedPort: Number(process.env.TAURI_DRIVER_PORT || 4445),
        env: {
            LUMO_E2E_TESTING: '1',
            LUMO_E2E_CONFIG_DIR: process.env.LUMO_E2E_CONFIG_DIR,
            LUMO_APPROVAL_TIMEOUT_MS: process.env.LUMO_APPROVAL_TIMEOUT_MS || '5000',
        },
        captureBackendLogs: true,
        captureFrontendLogs: true,
    }]],
    capabilities: [{ browserName: 'tauri', 'tauri:options': { application: process.env.LUMO_APP_BINARY },
        'wdio:tauriServiceOptions': { windowLabel: 'lumo-window' } }],
    framework: 'mocha',
    reporters: ['spec'],
    outputDir: process.env.LUMO_E2E_LOG_DIR,
    logLevel: process.env.WDIO_LOG_LEVEL || 'warn',
    waitforTimeout: 15_000,
    connectionRetryTimeout: 30_000,
    connectionRetryCount: 0,
    mochaOpts: { ui: 'bdd', timeout: 60_000 },
    before: async () => {
        // Explicit selection also disables 1.5.1's title-based focus recovery:
        // the native window title differs from the stub's document.title.
        await browser.tauri.switchWindow('lumo-window');
    },
};
