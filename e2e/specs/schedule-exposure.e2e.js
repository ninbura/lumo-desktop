const fs = require('node:fs');
const path = require('node:path');
const { textOf } = require('../support/waits');
const { request, connect, acceptedRun, createExecutor, startExecution, executionResult } = require('../support/schedules');
const { scratch, mcpId, prepareSecurityTools, disableSecurityTools, marker, callsFor } = require('../support/security');

describe('scheduled runs after reading untrusted content', () => {
    after(async () => disableSecurityTools());

    it('refuses outbound calls and writes despite auto-approve, without affecting a new run', async () => {
        await prepareSecurityTools();
        const { connectionId } = await connect();
        const readable = path.join(scratch, `scheduled-readable-${marker()}.txt`);
        fs.writeFileSync(readable, 'local read remains available');
        const connectors = ['bash', 'filesystem', mcpId];
        const run = await acceptedRun(connectionId, 'exposed-run');
        try {
            await createExecutor(run.runId, true, connectors);

            await startExecution('bash__run', 'scheduled', { command: 'echo untrusted-scheduled-content' });
            expect((await executionResult())[0].is_error).toBeFalsy();

            const input = { marker: marker() };
            const target = path.join(scratch, `scheduled-${marker()}.txt`);
            const operations = [
                [`${mcpId}__write`, input],
                [`${mcpId}__outbound`, input],
                // A hand-added server's closed-world/read-only claims are not trusted.
                [`${mcpId}__read`, input],
                ['filesystem__fs_write', { path: target, content: 'must not execute' }],
            ];
            for (const [name, args] of operations) {
                await startExecution(name, 'scheduled', args);
                const [result] = await executionResult();
                expect(result.is_error).toBe(true);
                expect(result.content).toContain('this scheduled run has read email or other untrusted');
                expect(await textOf('#pending')).toBe('0');
            }
            expect(callsFor(input.marker)).toEqual([]);
            expect(fs.existsSync(target)).toBe(false);

            // Local reads remain available in the affected run.
            await startExecution('filesystem__fs_read', 'scheduled', { path: readable });
            const [read] = await executionResult();
            expect(read.is_error).toBeFalsy();
            expect(read.content).toContain('local read remains available');
        } finally {
            await request('report', { connectionId, runId: run.runId, status: 'succeeded' });
        }

        const clean = await acceptedRun(connectionId, 'clean-run');
        try {
            await createExecutor(clean.runId, true, [mcpId]);
            const cleanInput = { marker: marker() };
            await startExecution(`${mcpId}__write`, 'scheduled', cleanInput);
            expect((await executionResult())[0].is_error).toBeFalsy();
            expect(callsFor(cleanInput.marker)).toEqual([{ tool: 'write', ...cleanInput }]);
            // Its result is untrusted, so even a claimed read-only call is now refused.
            const nextInput = { marker: marker() };
            await startExecution(`${mcpId}__read`, 'scheduled', nextInput);
            const [next] = await executionResult();
            expect(next.is_error).toBe(true);
            expect(next.content).toContain('this scheduled run has read email or other untrusted');
            expect(callsFor(nextInput.marker)).toEqual([]);
            expect(await textOf('#pending')).toBe('0');
        } finally {
            await request('report', { connectionId, runId: clean.runId, status: 'succeeded' });
        }

        const local = await acceptedRun(connectionId, 'local-run');
        try {
            // Enabled MCP servers outside this run's allowlist do not supply tool descriptions.
            await createExecutor(local.runId, true, ['filesystem']);
            const target = path.join(scratch, `scheduled-clean-${marker()}.txt`);
            await startExecution('filesystem__fs_write', 'scheduled', { path: target, content: 'clean run' });
            expect((await executionResult())[0].is_error).toBeFalsy();
            expect(fs.readFileSync(target, 'utf8')).toBe('clean run');
            expect(await textOf('#pending')).toBe('0');
        } finally {
            await request('report', { connectionId, runId: local.runId, status: 'succeeded' });
        }
    });
});
