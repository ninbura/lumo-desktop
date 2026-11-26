const fs = require('node:fs');
const path = require('node:path');
const { runTool } = require('../support/waits');
const {
    scratch, mcpId, prepareSecurityTools, disableSecurityTools, executeTool, setPolicy,
    approvalReason, resolve, marker, callsFor,
} = require('../support/security');

describe('tool approvals after untrusted content reaches the conversation', () => {
    before(async () => {
        await prepareSecurityTools();
        await setPolicy('bash', 'run', 'auto_approve');
        await setPolicy('filesystem', 'fs_write', 'auto_approve');
        await setPolicy('filesystem', 'fs_edit', 'auto_approve');
        await setPolicy(mcpId, 'write', 'auto_approve');
        // Real shell output reaches the production exposure tracker; no mock state.
        await executeTool('bash__run', { command: 'echo untrusted-e2e-content' });
        const read = { marker: marker() };
        await runTool(`${mcpId}__read`, read);
        expect(await approvalReason()).toEqual({ kind: 'untrusted_content', sources: ['shell', 'connector'] });
        expect((await resolve(true)).is_error).toBeFalsy();
        expect(callsFor(read.marker)).toEqual([{ tool: 'read', ...read }]);
    });

    after(async () => {
        await setPolicy('bash', 'run', 'ask');
        await setPolicy('filesystem', 'fs_write', 'ask');
        await setPolicy('filesystem', 'fs_edit', 'ask');
        await setPolicy(mcpId, 'write', 'ask');
        await disableSecurityTools();
    });

    it('gates even read-only hints from a hand-added server after untrusted content', async () => {
        for (const tool of ['read', 'write', 'outbound']) {
            const input = { marker: marker() };
            await runTool(`${mcpId}__${tool}`, input);
            expect(await approvalReason()).toEqual({ kind: 'untrusted_content', sources: ['shell', 'service', 'connector'] });
            expect(callsFor(input.marker)).toEqual([]);
            expect((await resolve(false)).is_error).toBe(true);
            expect(callsFor(input.marker)).toEqual([]);

            await runTool(`${mcpId}__${tool}`, input);
            await approvalReason();
            expect((await resolve(true)).is_error).toBeFalsy();
            expect(callsFor(input.marker)).toEqual([{ tool, ...input }]);
        }
    });

    it('gates file creation and edits, with disk changes only after approval', async () => {
        const target = path.join(scratch, `exposure-${marker()}.txt`);
        const write = { path: target, content: 'original' };
        await runTool('filesystem__fs_write', write);
        expect(await approvalReason()).toEqual({ kind: 'untrusted_write', sources: ['shell', 'service', 'connector'] });
        expect(fs.existsSync(target)).toBe(false);
        expect((await resolve(false)).is_error).toBe(true);
        expect(fs.existsSync(target)).toBe(false);

        await runTool('filesystem__fs_write', write);
        await approvalReason();
        expect((await resolve(true)).is_error).toBeFalsy();
        expect(fs.readFileSync(target, 'utf8')).toBe('original');

        const edit = { path: target, old_string: 'original', new_string: 'edited' };
        await runTool('filesystem__fs_edit', edit);
        await approvalReason();
        expect((await resolve(false)).is_error).toBe(true);
        expect(fs.readFileSync(target, 'utf8')).toBe('original');

        await runTool('filesystem__fs_edit', edit);
        await approvalReason();
        expect((await resolve(true)).is_error).toBeFalsy();
        expect(fs.readFileSync(target, 'utf8')).toBe('edited');
        await executeTool('filesystem__fs_read', { path: target });
    });
});
