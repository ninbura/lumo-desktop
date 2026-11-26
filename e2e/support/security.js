const fs = require('node:fs');
const path = require('node:path');
const { randomUUID } = require('node:crypto');
const { assertNoStubError, runTool, textOf, waitForPendingCount, waitForResult, waitForStubReady } = require('./waits');

const scratch = process.env.LUMO_E2E_SCRATCH;
const configDir = process.env.LUMO_E2E_CONFIG_DIR;
const mcpId = 'mcp-security';

async function setPolicy(connector, tool, policy) {
    await browser.execute(async (id, name, value) => {
        await window.LumoConnectors.setToolPolicy(id, name, value);
    }, connector, tool, policy);
}

async function prepareSecurityTools() {
    await waitForStubReady();
    // Discover through the production MCP transport, not a synthetic registry entry.
    await browser.execute(async () => {
        await window.LumoConnectors.setConnectorEnabled('mcp-security', true);
        await window.LumoConnectors.connectConnector('mcp-security');
        await window.LumoConnectors.loadRegistry();
        window.__e2eApprovals = [];
        if (!window.__e2eApprovalListener) {
            window.__e2eApprovalListener = (event) => window.__e2eApprovals.push(event.detail);
            window.addEventListener('lumo:tool-approval-request', window.__e2eApprovalListener);
        }
    });
}

async function disableSecurityTools() {
    await browser.execute(async () => {
        for (const id of ['mcp-security', 'mcp-legacy-cache']) {
            await window.LumoConnectors.setConnectorEnabled(id, false);
        }
    });
}

async function executeTool(name, input) {
    await runTool(name, input);
    const [result] = await waitForResult();
    expect(result.is_error).toBeFalsy();
    expect(await textOf('#pending')).toBe('0');
    return result;
}

async function approvalReason() {
    await waitForPendingCount(1);
    const requestId = await textOf('#pending-id');
    let approval;
    // Match the actual pending request, rather than whichever event arrived last.
    // Waiting also makes a missing observer event fail explicitly instead of
    // reusing a previous call's reason.
    await browser.waitUntil(async () => {
        await assertNoStubError();
        approval = await browser.execute((id) => (
            window.__e2eApprovals.find((event) => event.requestId === id) ?? null
        ), requestId);
        return approval !== null;
    }, { timeout: 20_000, timeoutMsg: `approval event for ${requestId} never arrived` });
    return approval.reason;
}

async function resolve(accepted) {
    await $(accepted ? '#approve' : '#deny').click();
    const [result] = await waitForResult();
    await waitForPendingCount(0);
    return result;
}

function marker() {
    return randomUUID();
}

function callsFor(marker) {
    const journal = path.join(scratch, 'mcp-journal.jsonl');
    if (!fs.existsSync(journal)) return [];
    return fs.readFileSync(journal, 'utf8').trim().split('\n').filter(Boolean)
        .map((line) => JSON.parse(line)).filter((entry) => entry.marker === marker);
}

module.exports = {
    scratch, configDir, mcpId, setPolicy, prepareSecurityTools, disableSecurityTools, executeTool,
    approvalReason, resolve, marker, callsFor,
};
