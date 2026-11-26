import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import vm from 'node:vm';
import { build } from 'esbuild';

const bundle = await build({
    entryPoints: [new URL('../webview/bridges/connectors.ts', import.meta.url).pathname],
    bundle: true,
    write: false,
    format: 'iife',
    platform: 'browser',
});
const source = bundle.outputFiles[0].text;
const reader = {
    name: 'tool_results__read',
    description: 'Read saved output',
    input_schema: { type: 'object' },
};
const browser = {
    id: 'browser',
    display_name: 'Browser',
    enabled: true,
    readiness: { state: 'ready' },
    status: { state: 'ready' },
    tools: [{ name: 'fetch' }],
};

function service(id, display_name, tools, description) {
    return {
        id,
        display_name,
        description,
        enabled: true,
        readiness: { state: 'ready' },
        status: { state: 'ready' },
        tools,
    };
}
const filler = 'Lorem ipsum dolor sit amet. '.repeat(20);
const luminance = service('luminance', 'Luminance', [
    { name: 'list_matters', description: `List matters. ${filler}`, input_schema: { type: 'object' } },
    { name: 'create_matter', description: `Create a matter. ${filler}`, input_schema: { type: 'object' } },
    ...Array.from({ length: 40 }, (_, i) => ({
        name: `get_report_${i}`,
        description: `Fetch report ${i}. ${filler}`,
        input_schema: { type: 'object', properties: { id: { type: 'integer' } } },
    })),
], 'Contract management:\n  matters, reports and approvals.');
const github = service('mcp-github', 'GitHub', [
    {
        name: 'createIssue',
        description: 'Open a new issue in a repository.',
        input_schema: { type: 'object', properties: { title: { type: 'string' } } },
    },
]);

async function bridge({ desktop = true, signedIn = true, invokeError, connectors = [] } = {}) {
    const calls = [];
    const events = [];
    const listeners = new Map();
    let enabled = true;
    const window = {
        __lumoAccountIdentity: { resolve: () => ({ userId: 'account-1' }) },
        dispatchEvent: (event) => events.push(event),
    };
    if (desktop)
        window.__TAURI_INTERNALS__ = {
            event: {
                listen: async (name, callback) => {
                    listeners.set(name, callback);
                },
            },
            invoke: async (command, args) => {
                calls.push({ command, args });
                if (command === 'get_tool_registry')
                    return signedIn ? [{ ...browser, enabled }, ...connectors] : [];
                if (command === 'get_builtin_tools') return signedIn ? [reader] : [];
                if (command === 'invoke_tool') {
                    if (invokeError) throw invokeError;
                    return { value: 'saved page' };
                }
                throw new Error(`Unexpected command: ${command}`);
            },
        };
    vm.runInNewContext(source, {
        window,
        console,
        navigator: { userAgent: '' },
        TextEncoder,
        fetch: async () => ({ ok: true, json: async () => (desktop ? [] : connectors) }),
        CustomEvent: class {
            constructor(type, options) {
                this.type = type;
                this.detail = options.detail;
            }
        },
    });
    await window.LumoConnectors.ready;
    return {
        window,
        calls,
        events,
        emit: (name, payload) => listeners.get(name)?.({ payload }),
        disableBrowser: async () => {
            enabled = false;
            await window.LumoConnectors.loadRegistry();
        },
    };
}

test('builtin reader is model-visible and executable without a UI-visible connector', async () => {
    const { window, calls, events, disableBrowser } = await bridge();
    assert.deepEqual(
        Array.from(window.LumoConnectors.getManifests(), (m) => m.id),
        ['browser'],
    );
    assert.deepEqual(
        Array.from(window.LumoConnectors.getConnectors(), (m) => m.id),
        ['browser'],
    );
    assert.ok(
        window.__TAURI__.external_tools
            .getTools()
            .some((t) => t.function.name === 'tool_results__read'),
    );
    assert.ok(events.every((e) => e.detail.manifests.every((m) => m.id !== 'tool_results')));
    assert.equal(calls.find((c) => c.command === 'get_builtin_tools').args.userId, 'account-1');
    const result = await window.__TAURI__.external_tools.executeToolCall({
        id: 'read-1',
        function: { name: 'tool_results__read', arguments: '{"result_id":"saved"}' },
    });
    assert.equal(result.content, 'saved page');
    assert.equal(calls.find((c) => c.command === 'invoke_tool').args.connectorId, 'tool_results');
    await disableBrowser();
    assert.ok(
        window.__TAURI__.external_tools
            .getTools()
            .some((t) => t.function.name === 'tool_results__read'),
    );
});
test('signed-out desktop advertises no builtin reader', async () => {
    const { window } = await bridge({ signedIn: false });
    assert.equal(window.__TAURI__.external_tools.getTools().length, 0);
});
test('serialized tool errors stay within the model-visible byte limit', async () => {
    const { window } = await bridge({ invokeError: `start:${'🦀\n'.repeat(20_000)}:end` });
    const result = await window.__TAURI__.external_tools.executeToolCall({
        id: 'read-error',
        function: { name: 'tool_results__read', arguments: '{}' },
    });
    assert.equal(result.is_error, true);
    assert.ok(Buffer.byteLength(result.content, 'utf8') <= 32_000);
    const { error } = JSON.parse(result.content);
    assert.ok(error.startsWith('start:'));
    assert.ok(error.endsWith(':end'));
    assert.match(error, /Tool error truncated; middle omitted/);
});
test('normal web does not acquire a native builtin or call desktop IPC', async () => {
    const { window, calls } = await bridge({ desktop: false });
    assert.equal(window.__TAURI__, undefined);
    assert.equal(window.LumoConnectors.getOpenAITools().length, 0);
    assert.equal(calls.length, 0);
});
const advertised = (window) =>
    Array.from(window.__TAURI__.external_tools.getTools(), (t) => t.function.name);
const call = (window, name, args) =>
    window.__TAURI__.external_tools.executeToolCall({
        id: 'call-1',
        function: { name, arguments: JSON.stringify(args) },
    });

test('a few connector tools are all sent with every request', async () => {
    const { window } = await bridge({ connectors: [github] });
    assert.deepEqual(advertised(window), [
        'browser__fetch',
        'mcp-github__createIssue',
        'tool_results__read',
    ]);
});
test('search keeps working after deferral switches off mid-conversation', async () => {
    const connectors = [luminance, github];
    const { window } = await bridge({ connectors });
    assert.ok(advertised(window).includes('tool_search__call_tool'));
    connectors.shift();
    await window.LumoConnectors.loadRegistry();
    assert.ok(!advertised(window).includes('tool_search__call_tool'));
    const result = await call(window, 'tool_search__call_tool', {
        name: 'mcp-github__createIssue',
        arguments: { title: 'Bug' },
    });
    assert.equal(result.content, 'saved page');
    const found = JSON.parse(
        (await call(window, 'tool_search__search_tools', { query: 'issue' })).content,
    );
    assert.equal(found.tools[0].name, 'mcp-github__createIssue');
});
test('too many connector tools are left out and found through search', async () => {
    const { window, events } = await bridge({ connectors: [luminance, github] });
    assert.deepEqual(advertised(window), [
        'browser__fetch',
        'tool_results__read',
        'tool_search__search_tools',
        'tool_search__call_tool',
    ]);
    assert.deepEqual(
        Array.from(events.at(-1).detail.openaiTools, (t) => t.function.name),
        advertised(window),
    );
    const search = window.__TAURI__.external_tools.getTools()[2].function.description;
    assert.match(
        search,
        /- Luminance \(connector "luminance"\): Contract management: matters, reports and approvals\.\n  Tools: list_matters, create_matter, get_\{report_0,report_1,.*,report_39\}$/m,
    );
    assert.match(search, /- GitHub \(connector "mcp-github"\).*\n  Tools: createIssue$/m);
    assert.match(search, /get_\{matter,task\} means get_matter and get_task/);
    assert.ok(!search.includes('Lorem ipsum'), 'tool descriptions stay out of the catalog');
    assert.ok(search.length < 2_000, `catalog is ${search.length} characters`);
    assert.ok(window.__TAURI__.external_tools.isNativeTool('tool_search__call_tool'));
    assert.ok(window.__TAURI__.external_tools.isNativeTool('luminance__list_matters'));
    assert.equal(window.LumoConnectors.getAvailableTools().length, 45);
});
test('a huge connector cannot crowd the others out of the catalog', async () => {
    const huge = service(
        'mcp-huge',
        'Huge',
        Array.from({ length: 400 }, (_, i) => ({ name: `operation_number_${i}` })),
    );
    const { window } = await bridge({ connectors: [huge, luminance, github] });
    const search = window.__TAURI__.external_tools.getTools()[2].function.description;
    assert.match(search, /Tools: operation_\{number_0,.*\}, and \d+ more\n/);
    assert.match(search, /Tools: list_matters, create_matter, get_\{report_0,.*,report_39\}$/m);
    assert.match(search, /Tools: createIssue$/m);
    assert.ok(search.length < 5_500, `catalog is ${search.length} characters`);
});
test('every real Luminance operation is named in the catalog', async () => {
    const table = JSON.parse(
        await readFile(
            new URL('../src-tauri/src/tools/connectors/luminance/operations.json', import.meta.url),
            'utf8',
        ),
    );
    const operations = Array.isArray(table) ? table : (table.operations ?? Object.values(table));
    const real = service(
        'luminance',
        'Luminance',
        operations.map(({ name }) => ({ name, description: 'x'.repeat(200) })),
    );
    const { window } = await bridge({ connectors: [real] });
    const search = window.__TAURI__.external_tools.getTools()[2].function.description;
    assert.ok(!search.includes(' more'), 'nothing left out');
    const expanded = search
        .match(/Tools: (.*)$/m)[1]
        .split(/, (?![^{]*\})/)
        .flatMap((part) => {
            const [, prefix, rests] = part.match(/^([^{]*)\{(.*)\}$/) ?? [];
            return rests ? rests.split(',').map((rest) => prefix + rest) : [part];
        });
    assert.deepEqual(expanded.sort(), operations.map(({ name }) => name).sort());
});
test('normal web sends gateway tools directly, however many there are', async () => {
    const { window } = await bridge({ desktop: false, connectors: [luminance, github] });
    const names = Array.from(window.LumoConnectors.getOpenAITools(), (t) => t.function.name);
    assert.equal(names.length, 43);
    assert.ok(names.includes('luminance__list_matters'));
    assert.ok(!names.some((name) => name.startsWith('tool_search__')));
    assert.ok(window.LumoConnectors.isAvailableTool('luminance__list_matters'));
});
test('search ranks tools by name and returns their parameters', async () => {
    const { window } = await bridge({ connectors: [luminance, github] });
    const found = async (args) =>
        JSON.parse((await call(window, 'tool_search__search_tools', args)).content);

    const issue = await found({ query: 'create issue' });
    assert.equal(issue.tools[0].name, 'mcp-github__createIssue');
    assert.deepEqual(issue.tools[0].parameters.properties, { title: { type: 'string' } });

    assert.equal((await found({ query: 'matters' })).tools[0].name, 'luminance__list_matters');

    const reports = await found({ query: 'report', limit: 3 });
    assert.equal(reports.tools.length, 3);
    assert.match(reports.note, /37 more tools match/);

    assert.equal((await found({ query: 'get_report_17' })).tools[0].name, 'luminance__get_report_17');

    const listed = await found({ connector: 'mcp-github' });
    assert.deepEqual(listed.tools.map((t) => t.name), ['mcp-github__createIssue']);

    const none = await found({ query: 'spaceship' });
    assert.deepEqual(none.tools, []);
    assert.match(none.note, /No tool matches/);
});
test('tools are looked up by the exact names in the catalog', async () => {
    const { window } = await bridge({ connectors: [luminance, github] });
    const found = async (args) =>
        JSON.parse((await call(window, 'tool_search__search_tools', args)).content);

    const exact = await found({ names: ['list_matters', 'mcp-github__createIssue', 'list_matters'] });
    assert.deepEqual(
        exact.tools.map((t) => t.name),
        ['luminance__list_matters', 'mcp-github__createIssue'],
    );
    assert.equal(exact.note, undefined);
    assert.deepEqual(exact.tools[1].parameters.properties, { title: { type: 'string' } });

    const missing = await found({ names: ['list_matter', 'get_report_3'], query: 'ignored' });
    assert.deepEqual(missing.tools.map((t) => t.name), ['luminance__get_report_3']);
    assert.match(missing.note, /Not found: list_matter\./);

    const scoped = await found({ names: ['createIssue'], connector: 'luminance' });
    assert.deepEqual(scoped.tools, []);
});
test('a bare name from the catalog can be called when only one connector has it', async () => {
    const twin = service('mcp-other', 'Other', [{ name: 'list_matters' }]);
    const { window, calls } = await bridge({ connectors: [luminance, github] });
    const result = await call(window, 'tool_search__call_tool', { name: 'createIssue', arguments: {} });
    assert.equal(result.content, 'saved page');
    assert.equal(calls.find((c) => c.command === 'invoke_tool').args.connectorId, 'mcp-github');

    const ambiguous = await bridge({ connectors: [luminance, twin] });
    const refused = await call(ambiguous.window, 'tool_search__call_tool', {
        name: 'list_matters',
        arguments: {},
    });
    assert.equal(refused.is_error, true);
    assert.equal(ambiguous.calls.filter((c) => c.command === 'invoke_tool').length, 0);
});
test('calling a found tool runs it natively under its own name', async () => {
    const { window, calls } = await bridge({ connectors: [luminance, github] });
    const result = await call(window, 'tool_search__call_tool', {
        name: 'mcp-github__createIssue',
        arguments: { title: 'Bug' },
    });
    assert.equal(result.content, 'saved page');
    assert.equal(result.name, 'tool_search__call_tool');
    assert.equal(result.tool_call_id, 'call-1');
    const invoked = calls.find((c) => c.command === 'invoke_tool').args;
    assert.equal(invoked.connectorId, 'mcp-github');
    assert.equal(invoked.toolName, 'createIssue');
    assert.deepEqual({ ...invoked.input }, { title: 'Bug' });

    for (const name of ['tool_search__call_tool', 'tool_search__search_tools', '', 'mcp-github__nope']) {
        const refused = await call(window, 'tool_search__call_tool', { name, arguments: {} });
        assert.equal(refused.is_error, true, name);
    }
    assert.equal(calls.filter((c) => c.command === 'invoke_tool').length, 1);
});
test('connector add and remove reach the web app as DOM events with only the kind', async () => {
    const { events, emit } = await bridge();
    events.length = 0;
    emit('telemetry:connector', { action: 'added', kind: 'mcp' });
    emit('telemetry:connector', { action: 'removed', kind: 'catalog' });
    emit('telemetry:connector', { action: 'renamed', kind: 'mcp' });
    emit('telemetry:connector', { action: 'added', kind: 'github', id: 'secret' });
    emit('telemetry:connector', undefined);
    assert.deepEqual(
        events.map((e) => [e.type, JSON.stringify(e.detail)]),
        [
            ['lumo:connector-added', '{"kind":"mcp"}'],
            ['lumo:connector-removed', '{"kind":"catalog"}'],
        ],
    );
});
