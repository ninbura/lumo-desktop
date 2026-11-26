const fs = require('node:fs');
const path = require('node:path');
const { textOf, waitForStubReady } = require('../support/waits');
const { accountId, request, connect, queueRun } = require('../support/schedules');

const configDir = process.env.LUMO_E2E_CONFIG_DIR;
const storePath = path.join(configDir, 'accounts', accountId, 'schedules.json');

describe('schedule bridge through the real desktop app', () => {
    it('recovers from unavailable event support before connecting', async () => {
        await waitForStubReady();
        const error = await browser.executeAsync((done) => {
            const tauri = window.__TAURI__;
            const internals = window.__TAURI_INTERNALS__;
            (async () => {
                try {
                    // Tauri's event exports are frozen. Shadow the namespace instead of
                    // mutating its methods, then restore it before returning to WebDriver.
                    window.__TAURI__ = { ...tauri, event: undefined };
                    window.__TAURI_INTERNALS__ = { ...internals, event: undefined };
                    await window.LumoSchedules.connect({ accountId: 'e2e-test-account', protocolVersion: 1 });
                    return 'unexpected success';
                } catch (error) {
                    return String(error);
                } finally {
                    window.__TAURI__ = tauri;
                    window.__TAURI_INTERNALS__ = internals;
                }
            })().then(done);
        });
        expect(error).toContain('Tauri event API not available');
        const failedSubscription = await browser.executeAsync((done) => {
            const tauri = window.__TAURI__;
            (async () => {
                try {
                    window.__TAURI__ = { ...tauri, event: {
                        ...tauri.event, listen: () => Promise.reject(new Error('test subscription failure')),
                    } };
                    await window.LumoSchedules.connect({ accountId: 'e2e-test-account', protocolVersion: 1 });
                    return 'unexpected success';
                } catch (error) {
                    return String(error);
                } finally {
                    window.__TAURI__ = tauri;
                }
            })().then(done);
        });
        expect(failedSubscription).toContain('test subscription failure');
        expect((await connect()).protocolVersion).toBe(1);
    });

    it('installs in the main document and stays out of child frames', async () => {
        const result = await browser.executeAsync((done) => {
            const iframe = document.createElement('iframe');
            iframe.onload = () => {
                const script = iframe.contentDocument.createElement('script');
                script.src = new URL('schedules.js', location.href).href;
                script.onload = () => {
                    const result = { main: !!window.LumoSchedules, child: !!iframe.contentWindow.LumoSchedules };
                    iframe.remove();
                    done(result);
                };
                script.onerror = () => { iframe.remove(); done({ error: 'child script did not load' }); };
                iframe.contentDocument.body.appendChild(script);
            };
            iframe.srcdoc = '<!doctype html><body></body>';
            document.body.appendChild(iframe);
        });
        expect(result).toEqual({ main: true, child: false });
    });

    it('validates accounts and protocol versions through native commands', async () => {
        await request('connect', { accountId, protocolVersion: 99 }, { error: 'unsupported schedule bridge protocol' });
        await request('connect', { accountId: 'another-account', protocolVersion: 1 }, { error: 'does not match' });
    });

    it('persists camelCase definitions using native timing and policy fields', async () => {
        const { connectionId } = await connect();
        const schedules = [
            { id: 'weekly', timingRevision: 2, enabled: true,
                timing: { kind: 'weekly', days: [1, 5], time: '09:00', timezone: 'Asia/Taipei' }, missedRunPolicy: 'run_once' },
            { id: 'monthly', timingRevision: 3, enabled: false,
                timing: { kind: 'monthly', days: [1, 31], time: '10:15', timezone: 'UTC' }, missedRunPolicy: 'catch_up' },
        ];
        await request('replace', { connectionId, schedules });
        const persisted = JSON.parse(fs.readFileSync(storePath, 'utf8'));
        for (const definition of schedules) {
            expect(persisted.schedules[definition.id].schedule).toEqual({
                id: definition.id, timing_revision: definition.timingRevision, enabled: definition.enabled,
                timing: definition.timing, missed_run_policy: definition.missedRunPolicy,
            });
        }
        await request('replace', { connectionId, schedules: [{ ...schedules[0], timing: {
            kind: 'daily', time: 'bad-time', timezone: 'UTC',
        } }] }, { error: 'time must be HH:MM' });
        expect(JSON.parse(fs.readFileSync(storePath, 'utf8'))).toEqual(persisted);
    });

    it('delivers persisted due work, preserves reads, and reconciles statuses after reconnect', async () => {
        const { connectionId } = await connect();
        const run = await queueRun(connectionId, 'schedule-round-trip');
        expect(run).toEqual({
            runId: expect.any(String), scheduleId: 'schedule-round-trip', timingRevision: 2,
            scheduledFor: expect.any(String), status: 'pending', updatedAt: expect.any(String),
            manual: false, late: false,
        });
        expect(await request('pending', { connectionId })).toEqual([run]);
        expect(await request('pending', { connectionId })).toEqual([run]);
        const persisted = JSON.parse(fs.readFileSync(storePath, 'utf8'));
        expect(persisted.runs[run.runId].status).toBe('pending');
        await browser.waitUntil(async () => JSON.parse(await textOf('#schedule-notices'))
            .some((notice) => notice.connectionId === connectionId), { timeout: 20_000 });
        await request('report', { connectionId, runId: run.runId, status: 'accepted' });
        await request('report', { connectionId, runId: run.runId, status: 'running' });
        expect(await request('pending', { connectionId })).toEqual([]);
        const reconnected = await connect();
        await request('pending', { connectionId }, { error: 'connection expired' });
        const unsettled = await request('unsettled', reconnected);
        expect(unsettled).toEqual([expect.objectContaining({ runId: run.runId, status: 'running' })]);
        await request('report', { connectionId: reconnected.connectionId, runId: run.runId, status: 'succeeded' });
        expect(await request('unsettled', reconnected)).toEqual([]);
    });

    it('fetches pending work on reconnect even when its notice was missed', async () => {
        const { connectionId } = await connect();
        await request('unsubscribe');
        const before = await textOf('#schedule-notices');
        const run = await queueRun(connectionId, 'schedule-missed-notice');
        expect(await textOf('#schedule-notices')).toBe(before);
        const reconnected = await connect();
        expect(await request('pending', reconnected)).toEqual([run]);
        await request('listen');
    });

    it('validates event payloads, isolates listeners, and unsubscribes over the real event transport', async () => {
        await connect();
        await request('unsubscribe');
        const notices = await browser.executeAsync((done) => {
            const received = [];
            const stopThrowing = window.LumoSchedules.onWorkAvailable(() => { throw new Error('test listener failure'); });
            const stop = window.LumoSchedules.onWorkAvailable((notice) => received.push(notice));
            const events = window.__TAURI__.event;
            (async () => {
                for (const payload of [null, {}, { connectionId: 123 }]) {
                    await events.emit('schedules:work_available', payload);
                }
                await events.emit('schedules:work_available', { connectionId: 'first' });
                // emit completes dispatch through the native event transport; wait for the callback.
                const deadline = Date.now() + 5000;
                while (!received.length && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 10));
                stop();
                await events.emit('schedules:work_available', { connectionId: 'second' });
                await new Promise((resolve) => setTimeout(resolve, 100));
                stopThrowing();
                done(received);
            })().catch((error) => done({ error: String(error) }));
        });
        expect(notices).toEqual([{ connectionId: 'first' }]);
        await request('listen');
    });
});
