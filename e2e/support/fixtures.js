const fs = require('node:fs');
const path = require('node:path');

// The caller owns a newly created temporary directory. Never reset an arbitrary path.
function seedFixtures(runDir) {
    const configParent = path.join(runDir, 'app-state');
    const config = path.join(configParent, 'config');
    const scratch = path.join(runDir, 'scratch');
    const account = path.join(config, 'accounts', 'e2e-test-account');
    fs.mkdirSync(account, { recursive: true });
    fs.mkdirSync(scratch);
    const write = (file, data) => fs.writeFileSync(file, JSON.stringify(data, null, 2));
    write(path.join(config, 'settings.json'), {
        search_shortcut: 'Ctrl+L', search_shortcut_enabled: false,
        launch_at_startup: false, settings_hotkey_enabled: false,
    });
    write(path.join(account, 'settings.json'), {
        filesystem_allowed_roots: [scratch, configParent], filesystem_write_enabled: true, bash_enabled: true,
    });
    fs.writeFileSync(path.join(config, 'e2e-protected.txt'), 'protected-e2e-sentinel\n');
    write(path.join(account, 'mcp_servers.json'), {
        servers: [
            {
                id: 'security', display_name: 'Security fixture', transport: 'stdio',
                command: process.execPath, args: [path.join(__dirname, '../fixtures/mcp-server.cjs')],
                env: { LUMO_E2E_MCP_JOURNAL: path.join(scratch, 'mcp-journal.jsonl') },
            },
            {
                id: 'legacy-cache', display_name: 'Offline legacy cache fixture', transport: 'stdio',
                command: process.execPath, args: ['-e', 'process.exit(1)'],
            },
        ],
    });
    // The offline peer cannot replace this legacy cache during discovery.
    const cache = path.join(account, 'mcp_tools');
    fs.mkdirSync(cache);
    write(path.join(cache, 'legacy-cache.json'), [
        { name: 'read', description: 'Old read tool', input_schema: { type: 'object' }, read_only: true, policy: 'auto_approve' },
        { name: 'write', description: 'Old write tool', input_schema: { type: 'object' }, read_only: false, policy: 'auto_approve' },
        { name: 'unannotated', description: 'Old unannotated tool', input_schema: { type: 'object' }, policy: 'auto_approve' },
    ]);
    write(path.join(account, 'connector_state.json'), {
        disabled_connectors: ['mcp-security', 'mcp-legacy-cache'],
        tool_policies: { echo: { ask: 'ask' }, filesystem: { fs_write: 'ask' } },
    });
    write(path.join(account, 'schedules.json'), { version: 1, schedules: {}, runs: {} });
    return { LUMO_E2E_CONFIG_DIR: config, LUMO_E2E_SCRATCH: scratch, LUMO_E2E_ESCAPE: path.join(runDir, 'escape.txt') };
}

module.exports = { seedFixtures };
