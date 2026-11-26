import {
    BUILTIN_CONNECTORS,
    CONNECTOR_CATEGORIES as SHARED_CONNECTOR_CATEGORIES,
    FEATURED_MCP_SERVERS,
    getFeaturedMcpServers,
    SETTINGS_CONNECTOR_TAB_PREFIX,
} from '../../shared/connector-catalog';

    const invoke = (command, args) => {
        if (command === 'connect_connector') {
            const attempt = pendingConnections.get(args.connectorId);
            if (attempt?.cancelled) return Promise.reject('authentication cancelled');
            if (attempt) attempt.connecting = true;
            return window.__TAURI_INTERNALS__.invoke(command, { ...args, attemptId: attempt?.id }).then(result => {
                if (attempt) attempt.connected = true;
                return result;
            });
        }
        return window.__TAURI_INTERNALS__.invoke(command, args);
    };
    const connectionErrors = new Map();
    const pendingConnections = new Map();

    function showConnectionCancel(id) {
        const attempt = pendingConnections.get(id);
        const button = document.getElementById('modal-disconnect');
        button.classList.remove('hidden');
        button.textContent = attempt.cancelled ? 'Cancelling…' : 'Cancel connection';
        button.disabled = attempt.cancelled;
        button.onclick = async () => {
            if (attempt.cancelled) return;
            attempt.cancelled = true;
            showConnectionCancel(id);
            if (attempt.connecting) {
                try {
                    await invoke('cancel_connector_connection', { connectorId: id, attemptId: attempt.id });
                } catch (e) {
                    attempt.cancelled = false;
                    showConnectionCancel(id);
                    setModalMsg(String(e), 'error');
                }
            }
        };
    }

    async function withConnectionAttempt(id, operation) {
        if (pendingConnections.has(id)) return;
        const attempt = { id: crypto.randomUUID(), cancelled: false, connecting: false };
        pendingConnections.set(id, attempt);
        const modalId = activeConnectorId === '__add_mcp__' ? activeConnectorId : id;
        const isActive = () => activeConnectorId === modalId;
        if (isActive()) {
            for (const button of ['modal-primary', 'modal-disconnect', 'modal-remove', 'refresh-tools'])
                document.getElementById(button).disabled = true;
            setModalMsg('Connecting…');
            showConnectionCancel(id);
        }
        try {
            return await operation();
        } finally {
            pendingConnections.delete(id);
            if (isActive()) {
                for (const button of ['modal-primary', 'modal-disconnect', 'modal-remove', 'refresh-tools'])
                    document.getElementById(button).disabled = false;
                refreshActiveModal();
                if (activeConnectorId === '__add_mcp__') {
                    document.getElementById('modal-disconnect').classList.add('hidden');
                }
                if (attempt.cancelled && !attempt.connected) setModalMsg('Connection cancelled. You can try again.');
            }
        }
    }

    const CONNECTOR_CATEGORIES = [{ id: 'all', label: 'All' }, ...SHARED_CONNECTOR_CATEGORIES];

    const withReference = (entry) => ({ ...entry, reference: entry.id });

    const CONNECTOR_CATALOG = Object.fromEntries(
        BUILTIN_CONNECTORS.map((entry) => [entry.id, withReference(entry)])
    );

    // Shared by the Google MCP servers. The OAuth client's project must have each server's MCP API
    // enabled (e.g. `gcloud beta services mcp enable bigquery.googleapis.com`).
    const GOOGLE_OAUTH_CLIENT = {
        // Google Desktop OAuth client ID is public by design.
        // nosemgrep: detected-google-oauth-url
        client_id: '1037595786787-j5gs6c7b3fqlvqm6v9mtc40f7s741eob.apps.googleusercontent.com',
        client_secret: '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',
    };

    const parseHttpsUrl = (value) => {
        try {
            const url = new URL(String(value || '').trim());
            return url.protocol === 'https:' ? url : null;
        } catch {
            return null;
        }
    };

    // A fixed list of endpoints, such as a vendor's regions; the first is the default.
    function choiceUrlInput(label, options) {
        const urls = options.map((o) => o.value);
        return {
            label,
            options,
            toUrl: (value) => (urls.includes(value) ? value : null),
            fromUrl: (url) => (urls.includes(url) ? url : urls[0]),
        };
    }

    // Tokens are only sent to the vendor's own hosts.
    function snowflakeMcpUrl(value) {
        const url = parseHttpsUrl(value);
        if (!url || !/\.snowflakecomputing\.com$/i.test(url.hostname)) return null;
        if (!/^\/api\/v2\/databases\/[^/]+\/schemas\/[^/]+\/mcp-servers\/[^/]+\/?$/i.test(url.pathname)) return null;
        return `${url.origin}${url.pathname.replace(/\/$/, '')}`;
    }

    function databricksMcpUrl(value) {
        const raw = String(value || '').trim();
        const url = parseHttpsUrl(/^https?:\/\//i.test(raw) ? raw : `https://${raw}`);
        if (!url || !/\.(cloud\.databricks\.com|gcp\.databricks\.com|azuredatabricks\.net)$/i.test(url.hostname)) {
            return null;
        }
        return `${url.origin}/ai-gateway/mcp-services/system.ai.dbsql`;
    }

    //   'dcr'          → no client_id; the server registers one dynamically (RFC 7591) + PKCE.
    //   'public'       → bundled client_id, PKCE, no secret.
    //   'confidential' → bundled client_id + client_secret (stored on connect).
    //                     Google Desktop clients are public clients; their client_secret is non-confidential.
    //   'byo'          → prompt the user for their own client_id (+ optional secret).
    //   'none'         → nothing to authorize (local stdio servers).
    const MCP_SETUP = {
        'mcp-chrome-devtools': {
            transport: 'stdio',
            command: 'npx',
            args: ['-y', 'chrome-devtools-mcp@1.9.0', "--no-usage-statistics"],
            auth: 'none',
        },
        'mcp-slack': {
          scopes: [
              "canvases:read",
              "canvases:write",
              "channels:history",
              "channels:read",
              "channels:write",
              "chat:write",
              "emoji:read",
              "files:read",
              "files:write",
              "groups:history",
              "groups:read",
              "groups:write",
              "im:history",
              "im:read",
              "im:write",
              "mpim:history",
              "mpim:read",
              "mpim:write",
              "reactions:read",
              "reactions:write",
              "search:read.files",
              "search:read.im",
              "search:read.mpim",
              "search:read.private",
              "search:read.public",
              "search:read.users",
              "users:read",
              "users:read.email"
            ],
            resource: 'https://mcp.slack.com',
            auth: 'public',
            client_id: '3465148148.12342856719909',
        },
        'mcp-atlassian': {
            scopes: [
                'read:me',
                'read:account',
                'offline_access',
                'read:jira:agent-interface',
                'write:jira:agent-interface',
                'search:jira:agent-interface',
                'read:confluence:agent-interface',
                'write:confluence:agent-interface',
                'search:confluence:agent-interface',
                'search:rovo:agent-interface',
            ],
            resource: null,
            auth: 'dcr',
        },
        'mcp-dropbox': {
            scopes: [],
            resource: null,
            auth: 'dcr',
            authorize_params: { token_access_type: 'offline' },
        },
        'mcp-google-drive': {
            scopes: ['https://www.googleapis.com/auth/drive'],
            resource: null,
            auth: 'confidential',
            client_id: GOOGLE_OAUTH_CLIENT.client_id,
            client_secret: GOOGLE_OAUTH_CLIENT.client_secret,
            authorize_params: { access_type: 'offline', prompt: 'consent' },
        },
        'mcp-polixis': {
            auth: 'headers',
            headers: [
                { name: 'Authorization', prefix: 'Bearer ', label: 'Bearer token' },
                { name: 'x-api-key', prefix: '', label: 'API key' },
            ],
        },
        'mcp-greenhouse': {
            scopes: [],
            resource: null,
            auth: 'dcr',
        },
        'mcp-figma': {
            auth: 'none',
            connectHint:
                'Connecting to the Figma desktop app… If Connect fails, press Cmd+K (Ctrl+K on Windows) in a Figma Design file and run “Enable desktop MCP server”.',
        },
        'mcp-zendesk': {
            transport: 'stdio',
            command: 'npx',
            args: ['-y', '@fruggr/zendesk-mcp-server@3.3.0'],
            auth: 'none',
            subdomain: true,
            connectHint:
                'Starting the local Zendesk MCP server… Sign in in your browser the first time you use a Zendesk tool.',
        },
        'mcp-grafana': {
            grafana: true,
            scopes: [],
            resource: null,
            auth: 'dcr',
        },
        'mcp-linear': {
            scopes: [],
            resource: null,
            auth: 'dcr',
        },
        'mcp-granola': {
            scopes: [],
            resource: null,
            auth: 'dcr',
        },
        'mcp-wispr-flow': {
            scopes: [],
            resource: null,
            auth: 'dcr',
        },
        'mcp-brandfetch': {
            scopes: [],
            resource: null,
            auth: 'dcr',
        },
        'mcp-asana': {
            scopes: [],
            resource: 'https://mcp.asana.com/v2',
            auth: 'byo',
            byoHint:
                'Asana needs an MCP app of your own. In the Asana developer console, create an MCP app with the redirect URLs http://127.0.0.1:9876/callback, :9877/callback and :9878/callback, allow your workspace, then enter its client ID and secret.',
        },
        'mcp-github': {
            auth: 'headers',
            headers: [{ name: 'Authorization', prefix: 'Bearer ', label: 'Personal access token' }],
        },
        'mcp-pagerduty': {
            auth: 'headers',
            headers: [{ name: 'Authorization', prefix: 'Token token=', label: 'API token' }],
            urlInput: choiceUrlInput('Service region', [
                { value: 'https://mcp.pagerduty.com/mcp', label: 'US' },
                { value: 'https://mcp.eu.pagerduty.com/mcp', label: 'EU' },
            ]),
        },
        'mcp-datadog': {
            scopes: [],
            resource: null,
            auth: 'dcr',
            urlInput: choiceUrlInput('Datadog site', [
                { value: 'https://mcp.datadoghq.com/v1/mcp', label: 'US1 (app.datadoghq.com)' },
                { value: 'https://mcp.us3.datadoghq.com/v1/mcp', label: 'US3 (us3.datadoghq.com)' },
                { value: 'https://mcp.us5.datadoghq.com/v1/mcp', label: 'US5 (us5.datadoghq.com)' },
                { value: 'https://mcp.datadoghq.eu/v1/mcp', label: 'EU1 (app.datadoghq.eu)' },
                { value: 'https://mcp.uk1.datadoghq.com/v1/mcp', label: 'UK1 (uk1.datadoghq.com)' },
                { value: 'https://mcp.ap1.datadoghq.com/v1/mcp', label: 'AP1 (ap1.datadoghq.com)' },
                { value: 'https://mcp.ap2.datadoghq.com/v1/mcp', label: 'AP2 (ap2.datadoghq.com)' },
            ]),
        },
        'mcp-snowflake': {
            auth: 'headers',
            headers: [{ name: 'Authorization', prefix: 'Bearer ', label: 'Programmatic access token' }],
            request_headers: { 'X-Snowflake-Authorization-Token-Type': 'PROGRAMMATIC_ACCESS_TOKEN' },
            urlInput: {
                label: 'MCP server URL',
                placeholder: 'https://<account>.snowflakecomputing.com/api/v2/databases/<db>/schemas/<schema>/mcp-servers/<name>',
                hint: 'Create an MCP server in Snowflake, then paste its URL. Use a token for a role limited to that server.',
                toUrl: snowflakeMcpUrl,
                fromUrl: (url) => url || '',
            },
        },
        'mcp-databricks': {
            auth: 'headers',
            headers: [{ name: 'Authorization', prefix: 'Bearer ', label: 'Personal access token' }],
            urlInput: {
                label: 'Workspace URL',
                placeholder: 'https://dbc-1234abcd-5678.cloud.databricks.com',
                hint: 'Uses the Databricks SQL MCP server of your workspace.',
                toUrl: databricksMcpUrl,
                fromUrl: (url) => (url ? new URL(url).origin : ''),
            },
        },
        'mcp-bigquery': {
            scopes: ['https://www.googleapis.com/auth/bigquery'],
            resource: null,
            auth: 'confidential',
            client_id: GOOGLE_OAUTH_CLIENT.client_id,
            client_secret: GOOGLE_OAUTH_CLIENT.client_secret,
            authorize_params: { access_type: 'offline', prompt: 'consent' },
        },
        'mcp-salesforce': {
            scopes: ['mcp_api', 'refresh_token'],
            resource: null,
            auth: 'byo',
            byoHint:
                'Salesforce needs an External Client App of your own. In Setup, create one with OAuth enabled, the callback URLs http://127.0.0.1:9876/callback, :9877/callback and :9878/callback, and the mcp_api and refresh_token scopes. Then enter its consumer key as the client ID; leave the secret blank unless the app requires one.',
            urlInput: choiceUrlInput('Org type', [
                { value: 'https://api.salesforce.com/platform/mcp/v1/platform/sobject-all', label: 'Production or Developer' },
                { value: 'https://api.salesforce.com/platform/mcp/v1/sandbox/platform/sobject-all', label: 'Sandbox or scratch' },
            ]),
        },
        'mcp-stripe': {
            scopes: ['mcp'],
            resource: null,
            auth: 'dcr',
        },
        'mcp-alpha-vantage': {
            scopes: ['alphavantage:read'],
            resource: null,
            auth: 'dcr',
        },
        'mcp-tavily': {
            scopes: ['openid', 'offline_access'],
            resource: null,
            auth: 'dcr',
        },
        'mcp-alphaxiv': {
            scopes: ['openid', 'profile', 'email', 'offline_access'],
            resource: null,
            auth: 'dcr',
        },
        'mcp-papernexus': {
            scopes: ['papers:read', 'library:read', 'offline_access'],
            resource: null,
            auth: 'dcr',
        },
        'mcp-sonos': {
            scopes: ['playback-control-all', 'partner-content:read'],
            resource: null,
            auth: 'dcr',
        },
        'mcp-pubmed': { auth: 'none' },
        'mcp-biorxiv': { auth: 'none' },
        'mcp-clinical-trials': { auth: 'none' },
        'mcp-chembl': { auth: 'none' },
        'mcp-open-targets': { auth: 'none' },
        'mcp-13f-ai': {
            scopes: [],
            resource: null,
            auth: 'dcr',
        },
    };

    const MCP_CATALOG = Object.fromEntries(
        FEATURED_MCP_SERVERS.map((entry) => [entry.id, { ...withReference(entry), ...MCP_SETUP[entry.id] }])
    );

    const FEATURED_MCP_IDS = Object.keys(MCP_CATALOG);

    let appSettings = {};
    let filesystemRoots = [];
    let filesystemWriteEnabled = false;
    let bashEnabled = false;
    let manifests = [];
    let desktopFeatureFlags = { experimentalConnectors: false };
    let mcpServers = [];
    let loadError = null;
    let secureStorageAvailable = true;
    let activeConnectorId = null;
    let connectorCategoryFilter = 'all';
    let connectorSearchQuery = '';
    let recordingShortcut = false;
    let resetInProgress = false;

    const MODIFIER_CODES = /^(Control|Shift|Alt|Meta)(Left|Right)$/;
    const META_MODIFIER = navigator.userAgent.toLowerCase().includes('mac') ? 'Cmd' : 'Super';

    function shortcutModifiers(e) {
        const modifiers = [];
        if (e.ctrlKey) modifiers.push('Ctrl');
        if (e.altKey) modifiers.push('Alt');
        if (e.shiftKey) modifiers.push('Shift');
        if (e.metaKey) modifiers.push(META_MODIFIER);
        return modifiers;
    }

    function shortcutKeyName(code) {
        return code.replace(/^(Key|Digit)(?=.$)/, '');
    }

    function pendingShortcutLabel(e) {
        const modifiers = shortcutModifiers(e);
        return modifiers.length ? modifiers.join('+') + '+…' : 'Press keys…';
    }

    function buildSettingsPayload() {
        const shortcutInput = document.getElementById('search-shortcut');
        return {
            search_shortcut: shortcutInput.value || appSettings.search_shortcut,
            search_shortcut_enabled: document.getElementById('search-shortcut-enabled').checked,
            launch_at_startup: document.getElementById('launch-startup').checked,
            filesystem_allowed_roots: filesystemRoots,
            filesystem_write_enabled: filesystemWriteEnabled,
            bash_enabled: bashEnabled,
            settings_hotkey_enabled: document.getElementById('settings-hotkey').checked,
        };
    }

    function refreshActiveModal() {
        if (!activeConnectorId) return;
        const manifest = manifests.find(m => m.id === activeConnectorId);
        if (!manifest) return;
        const connected = isConnected(manifest);
        syncModalStatusFromManifest(manifest, activeConnectorId);
        renderToolsList(activeConnectorId, manifest);
        updateModalActions(activeConnectorId, manifest, connected);
    }

    async function persistAppSettings({ refreshModal = false } = {}) {
        const settings = buildSettingsPayload();
        await invoke('save_app_settings', { settings });
        appSettings = settings;
        document.getElementById('current-shortcut').textContent = settings.search_shortcut;
        document.getElementById('search-shortcut').value = '';
        await refreshData();
        if (refreshModal) {
            refreshActiveModal();
        }
    }

    function isConnected(manifest) {
        return manifest?.status?.state === 'ready';
    }

    /** Maps `ConnectorStatus` to the dot's modifier class. The judgement itself is made in Rust. */
    function statusDotClass(manifest) {
        switch (manifest?.status?.state) {
            case 'ready': return 'status-dot connected';
            case 'warning': return 'status-dot warning';
            default: return 'status-dot';
        }
    }

    function statusLabel(manifest) {
        const status = manifest?.status;
        switch (status?.state) {
            case 'ready': return 'Connected';
            case 'warning': return 'Connected with issues';
            case 'not_ready': {
                const reason = status.reason;
                if (reason === 'Secure storage is unavailable') return 'Needs attention';
                if (manifest?.activity?.state === 'failed') return 'Connection failed';
                if (!reason || reason === 'Not connected' || reason === 'Not connected yet') return 'Not connected';
                return 'Connection failed';
            }
            default: return 'Not connected';
        }
    }

    function connectorFailureDetail(manifest) {
        if (manifest?.activity?.state === 'failed') return manifest.activity.error;
        const status = manifest?.status;
        if (status?.state === 'not_ready' && status.reason && status.reason !== 'Not connected' && status.reason !== 'Not connected yet') {
            return status.reason;
        }
        if (status?.state === 'warning' && status.error) return status.error;
        return null;
    }

    function humanizeConnectorError(connectorId, raw) {
        const text = String(raw || '');
        if (/secure storage is unavailable/i.test(text)) {
            return {
                title: 'Cannot access saved credentials',
                body: 'Allow Lumo to access your OS keychain, then try again.',
            };
        }
        if (connectorId === 'mcp-figma') {
            if (/3845|127\.0\.0\.1|connection refused|error sending request|could not reach/i.test(text)) {
                return {
                    title: 'Figma desktop MCP is not running',
                    body: 'In the Figma desktop app, open a Design file, press Cmd+K (Ctrl+K on Windows), search for “MCP” and choose “Enable desktop MCP server”. Leave Figma open and try Connect again.',
                };
            }
        }
        if (/client registration failed.*403|Registration failed: HTTP 403/i.test(text)) {
            return {
                title: 'This MCP server does not accept Lumo yet',
                body: 'The provider rejected OAuth client registration. Use a local/desktop MCP option when available, or ask the provider to allow this client.',
            };
        }
        if (/could not reach|initialize failed|connection refused|error sending request/i.test(text)) {
            return {
                title: 'Could not reach the MCP server',
                body: 'Check that the server is running and the URL is correct, then try Connect again.',
            };
        }
        return { title: 'Connection failed', body: text };
    }

    function renderStatusMessage(el, connectorId, raw, type = 'error') {
        if (!raw) {
            el.textContent = '';
            el.className = 'status-msg';
            return;
        }
        if (type !== 'error') {
            el.textContent = raw;
            el.className = `status-msg ${type}`;
            return;
        }
        const { title, body } = humanizeConnectorError(connectorId, raw);
        el.className = 'status-msg error';
        el.replaceChildren();
        const titleEl = document.createElement('p');
        titleEl.className = 'status-msg-title';
        titleEl.textContent = title;
        el.appendChild(titleEl);
        const bodyEl = document.createElement('p');
        bodyEl.className = 'status-msg-body';
        bodyEl.textContent = body;
        el.appendChild(bodyEl);
        if (body !== raw) {
            const details = document.createElement('details');
            details.className = 'status-details';
            const summary = document.createElement('summary');
            summary.textContent = 'Technical details';
            const pre = document.createElement('pre');
            pre.textContent = raw;
            details.append(summary, pre);
            el.appendChild(details);
        }
    }

    function syncModalStatusFromManifest(manifest, connectorId) {
        document.getElementById('modal-status-dot').className = statusDotClass(manifest);
        document.getElementById('modal-status-text').textContent = statusLabel(manifest);
        if (pendingConnections.has(connectorId)) {
            renderStatusMessage(document.getElementById('modal-status-msg'), connectorId, 'Connecting…', '');
            return;
        }
        if (isConnected(manifest)) connectionErrors.delete(connectorId);
        const detail = connectionErrors.get(connectorId) || connectorFailureDetail(manifest);
        renderStatusMessage(
            document.getElementById('modal-status-msg'),
            connectorId,
            detail,
            detail ? 'error' : ''
        );
    }

    function connectorUsesKeychain(id) {
        const preset = MCP_CATALOG[id];
        if (preset?.auth === 'none') return false;
        const server = mcpServers.find(s => `mcp-${s.id}` === id);
        if (server?.transport === 'http' && server.auth?.type === 'none') return false;
        if (id === 'filesystem' || id === 'browser' || id === 'bash') return false;
        return true;
    }

    function activatedToolCount(manifest) {
        const tools = manifest?.tools || [];
        if (!isConnected(manifest) || tools.length === 0) return null;
        return tools.filter((t) => (t.policy || 'auto_approve') !== 'disable').length;
    }

    function connectorFunctionsLabel(manifest) {
        const count = activatedToolCount(manifest);
        if (count === null) return '';
        const noun = count === 1 ? 'function' : 'functions';
        return `${count} ${noun} active`;
    }


    /** Names of a stdio server's keychain-held variables. The values are not sent to this page. */
    function secretEnvNames(server) {
        return server?.transport === 'stdio' ? (server.secret_env || []) : [];
    }

    function mcpTransportSummary(server) {
        if (!server) return '';
        if (server.transport === 'http') return server.url || '';
        return `${server.command || ''} ${(server.args || []).join(' ')}`.trim();
    }

    function getCatalogEntry(id) {
        if (MCP_CATALOG[id]) return MCP_CATALOG[id];
        if (CONNECTOR_CATALOG[id]) return CONNECTOR_CATALOG[id];
        const mcp = mcpServers.find(s => `mcp-${s.id}` === id);
        if (mcp) {
            return {
                category: 'custom',
                icon: '🔗',
                iconSrc: 'images/connectors/mcp.svg',
                name: mcp.display_name,
                description: mcp.description || `MCP server — ${mcpTransportSummary(mcp)}`,
                reference: id,
            };
        }
        const manifest = manifests.find(m => m.id === id);
        return {
            category: 'custom',
            icon: '🔌',
            name: manifest?.display_name || id,
            description: 'Custom connector',
            reference: id,
        };
    }

    function connectorCategory(id) {
        return getCatalogEntry(id).category || 'custom';
    }

    function listConnectorIds() {
        const ids = [...BUILTIN_CONNECTORS.map(entry => entry.id),
            ...getFeaturedMcpServers(desktopFeatureFlags.experimentalConnectors).map(entry => entry.id)];
        for (const server of mcpServers) {
            const id = `mcp-${server.id}`;
            if (!FEATURED_MCP_IDS.includes(id)) ids.push(id);
        }
        return ids;
    }

    function connectorMatchesCategoryFilter(id) {
        return connectorCategoryFilter === 'all' || connectorCategory(id) === connectorCategoryFilter;
    }

    function connectorMatchesSearch(id) {
        const terms = connectorSearchQuery.toLowerCase().split(/\s+/).filter(Boolean);
        if (terms.length === 0) return true;
        const entry = getCatalogEntry(id);
        const categoryLabel = CONNECTOR_CATEGORIES.find((cat) => cat.id === connectorCategory(id))?.label || '';
        const haystack = [entry.name, entry.description, categoryLabel, id].join(' ').toLowerCase();
        return terms.every((term) => haystack.includes(term));
    }

    function visibleConnectorCategories() {
        const ids = listConnectorIds();
        const used = new Set(ids.map(connectorCategory));
        return CONNECTOR_CATEGORIES.filter((cat) => cat.id === 'all' || used.has(cat.id));
    }

    /** Parses `KEY=VALUE` lines into an object. Returns null if a line has no name or no `=`. */
    function parseEnvLines(raw) {
        const entries = {};
        for (const line of String(raw || '').split('\n')) {
            const trimmed = line.trim();
            if (!trimmed) continue;
            const eq = trimmed.indexOf('=');
            if (eq <= 0) return null;
            entries[trimmed.slice(0, eq).trim()] = trimmed.slice(eq + 1).trim();
        }
        return entries;
    }

    function infoParagraph(text) {
        const p = document.createElement('p');
        p.className = 'info';
        p.textContent = text;
        return p;
    }

    function formGrid(...fields) {
        const grid = document.createElement('div');
        grid.className = 'form-grid';
        grid.append(...fields);
        return grid;
    }

    function fieldHint(text) {
        const p = infoParagraph(text);
        p.classList.add('field-hint');
        return p;
    }

    function formFieldWithControl(labelText, control) {
        const wrap = document.createElement('div');
        wrap.className = 'form-field';
        const label = document.createElement('label');
        label.textContent = labelText;
        wrap.append(label, control);
        return wrap;
    }

    function checkboxGroup({ id, checked, labelText, disabled = false, marginTop }) {
        const group = document.createElement('div');
        group.className = 'checkbox-group';
        if (marginTop) group.style.marginTop = marginTop;
        const input = document.createElement('input');
        input.type = 'checkbox';
        input.id = id;
        input.checked = checked;
        if (disabled) input.disabled = true;
        const label = document.createElement('label');
        label.htmlFor = id;
        label.textContent = labelText;
        group.append(input, label);
        return group;
    }

    function securitySelect(id, current) {
        const select = document.createElement('select');
        select.id = id;
        for (const [value, label] of [
            ['start_tls', 'STARTTLS'],
            ['tls', 'SSL/TLS'],
        ]) {
            const option = document.createElement('option');
            option.value = value;
            option.textContent = label;
            option.selected = current === value;
            select.appendChild(option);
        }
        return select;
    }

    function createFieldRow(labelText, valueText, valueClass = '') {
        const row = document.createElement('div');
        row.className = 'field-row';
        const label = document.createElement('label');
        label.textContent = labelText;
        const value = document.createElement('span');
        if (valueClass) value.className = valueClass;
        value.textContent = valueText;
        row.append(label, value);
        return row;
    }

    function appendSetupCard(parent, { leadParts, steps, labelText = 'Before you connect' }) {
        const card = document.createElement('div');
        card.className = 'setup-card';
        const lead = document.createElement('p');
        lead.className = 'setup-card-lead';
        for (const part of leadParts) {
            if (typeof part === 'string') lead.append(part);
            else lead.appendChild(part);
        }
        card.appendChild(lead);
        const heading = document.createElement('p');
        heading.className = 'setup-card-label';
        heading.textContent = labelText;
        card.appendChild(heading);
        const list = document.createElement('ol');
        list.className = 'setup-steps';
        for (const step of steps) {
            const li = document.createElement('li');
            li.textContent = step;
            list.appendChild(li);
        }
        card.appendChild(list);
        parent.appendChild(card);
    }

    function renderZendeskMcpAuth(container, { savedSubdomain, preset, connected, subdomain }) {
        container.replaceChildren();
        if (!connected) {
            const link = document.createElement('a');
            link.href = 'https://github.com/fruggr/zendesk-mcp-server';
            link.textContent = 'Zendesk MCP';
            appendSetupCard(container, {
                leadParts: ['Local ', link, ' on this device (Node.js 22+). Connect saves your subdomain; OAuth runs when you first use a Zendesk tool.'],
                labelText: 'Before first tool use',
                steps: [
                    'Create a public OAuth client in Zendesk Admin → APIs → OAuth clients.',
                    'Identifier: {subdomain}_zendesk (example: acme_zendesk).',
                    'Redirect URL: http://localhost:27439/callback (use localhost, not 127.0.0.1).',
                    'Scopes: read and write.',
                ],
            });
            const field = document.createElement('div');
            field.className = 'form-field';
            const fieldLabel = document.createElement('label');
            fieldLabel.textContent = 'Subdomain';
            const row = document.createElement('div');
            row.style.display = 'flex';
            row.style.alignItems = 'center';
            row.style.gap = '6px';
            const input = document.createElement('input');
            input.type = 'text';
            input.id = 'mcp-subdomain';
            input.placeholder = 'your-company';
            input.value = savedSubdomain || '';
            input.style.flex = '1';
            const suffix = document.createElement('span');
            suffix.textContent = '.zendesk.com';
            row.append(input, suffix);
            field.append(fieldLabel, row);
            container.append(
                field,
                createFieldRow('Command', preset.command),
                createFieldRow('Args', (preset.args || []).join(' '), 'field-value-mono')
            );
            return;
        }
        container.append(
            createFieldRow('Subdomain', `${subdomain || '—'}.zendesk.com`),
        );
        const info = document.createElement('p');
        info.className = 'info';
        info.textContent = 'Server is running. OAuth runs on the first Zendesk tool use.';
        container.appendChild(info);
    }

    function renderFigmaMcpAuth(container, { preset, connected }) {
        container.replaceChildren();
        if (!connected) {
            const link = document.createElement('a');
            link.href = 'https://developers.figma.com/docs/figma-mcp-server/local-server-installation/';
            link.textContent = 'Figma desktop MCP server';
            appendSetupCard(container, {
                leadParts: ['Connects to the ', link, ' on this computer (not Figma’s cloud OAuth server).'],
                steps: [
                    'Open a Design file in the Figma desktop app.',
                    'Press Cmd+K (Ctrl+K on Windows) to open Quick actions.',
                    'Search for “MCP” and choose “Enable desktop MCP server”.',
                    'Leave Figma open, then click Connect.',
                ],
            });
            container.appendChild(createFieldRow('Server URL', preset.url, 'field-value-mono'));
            return;
        }
        container.appendChild(createFieldRow('Server URL', preset.url, 'field-value-mono'));
        const info = document.createElement('p');
        info.className = 'info';
        info.textContent = 'Connected to Figma desktop. Keep Figma open with the desktop MCP server enabled while you use Figma tools.';
        container.appendChild(info);
    }

    function renderPublicMcpAuth(container, preset, connected) {
        container.replaceChildren();
        container.appendChild(infoParagraph(connected
            ? 'Connected. This is a public server, so there is no account to sign in to.'
            : 'Public server — no sign-in needed. Your requests are sent to the server below.'));
        container.appendChild(createFieldRow('Server URL', preset.url, 'field-value-mono'));
    }

    function mcpSubdomainFromUrl(url) {
        if (!url) return '';
        const match = url.match(/^https:\/\/([^.]+)\.zendesk\.com\//);
        return match?.[1] || '';
    }

    function mcpSubdomainFromZendeskServer(server) {
        if (server?.env?.ZENDESK_SUBDOMAIN) {
            return server.env.ZENDESK_SUBDOMAIN;
        }
        return mcpSubdomainFromUrl(server?.url);
    }

    // Presets whose endpoint depends on the user's account, workspace or region.
    function appendPresetUrlField(parent, preset, server) {
        const input = preset.urlInput;
        if (!input) return;
        const saved = server?.url ? input.fromUrl(server.url) : '';
        let control;
        if (input.options) {
            control = document.createElement('select');
            control.id = 'mcp-instance-url';
            for (const optionSpec of input.options) {
                const option = document.createElement('option');
                option.value = optionSpec.value;
                option.textContent = optionSpec.label;
                option.selected = optionSpec.value === saved;
                control.appendChild(option);
            }
        } else {
            control = document.createElement('input');
            control.type = 'text';
            control.id = 'mcp-instance-url';
            if (input.placeholder) control.placeholder = input.placeholder;
            control.value = saved;
        }
        parent.appendChild(formFieldWithControl(input.label, control));
        if (input.hint) parent.appendChild(infoParagraph(input.hint));
    }

    // The endpoint to save for a preset; reports an error and returns null when the input is invalid.
    function readPresetUrl(preset) {
        if (!preset.urlInput) return preset.url;
        const url = preset.urlInput.toUrl(document.getElementById('mcp-instance-url')?.value);
        if (!url) {
            const example = preset.urlInput.placeholder ? `, like ${preset.urlInput.placeholder}` : '';
            setModalMsg(`Enter a valid ${preset.urlInput.label.toLowerCase()}${example}.`, 'error');
        }
        return url;
    }

    function mcpPresetUrl(preset, subdomain) {
        if (preset.url) return preset.url;
        if (preset.urlTemplate && subdomain) {
            return preset.urlTemplate.replace('{subdomain}', subdomain);
        }
        return '';
    }

    const GRAFANA_INSTANCE_RUNNERS = {
        docker: {
            command: 'docker',
            args: [
                'run', '--rm', '-i',
                '-e', 'GRAFANA_URL',
                '-e', 'GRAFANA_SERVICE_ACCOUNT_TOKEN',
                'grafana/mcp-grafana',
                '-t', 'stdio',
            ],
        },
        uvx: {
            command: 'uvx',
            args: ['mcp-grafana'],
        },
    };

    function mcpGrafanaModeFromServer(server) {
        if (server?.transport === 'stdio') return 'instance';
        if (server?.transport === 'http') return 'cloud';
        return 'instance';
    }

    function mcpGrafanaRunnerFromServer(server) {
        if (server?.command === 'uvx') return 'uvx';
        return 'docker';
    }

    function mcpGrafanaTlsSkipFromServer(server) {
        return (server?.args || []).includes('--tls-skip-verify');
    }

    function grafanaInstanceServerConfig(preset, grafanaUrl, runner, skipTlsVerify) {
        const launch = GRAFANA_INSTANCE_RUNNERS[runner] || GRAFANA_INSTANCE_RUNNERS.docker;
        const args = [...launch.args];
        if (skipTlsVerify) {
            if (runner === 'docker') {
                const imageIdx = args.indexOf('grafana/mcp-grafana');
                if (imageIdx >= 0) args.splice(imageIdx + 1, 0, '--tls-skip-verify');
            } else {
                args.push('--tls-skip-verify');
            }
        }
        return {
            id: preset.server_id,
            display_name: preset.name,
            transport: 'stdio',
            command: launch.command,
            args,
            env: { GRAFANA_URL: grafanaUrl },
            secret_env: ['GRAFANA_SERVICE_ACCOUNT_TOKEN'],
        };
    }

    function mcpGrafanaStackFromServer(server) {
        const headerUrl = server?.request_headers?.['X-Grafana-URL'];
        if (!headerUrl) return '';
        const match = headerUrl.match(/^https:\/\/([^.]+)\.grafana\.net\/?$/);
        return match?.[1] || '';
    }

    function mcpGrafanaRequestHeaders(stack) {
        const trimmed = stack?.trim();
        if (!trimmed) return undefined;
        return { 'X-Grafana-URL': `https://${trimmed}.grafana.net` };
    }

    function normalizeGrafanaUrl(url) {
        let value = String(url || '').trim();
        if (!value) return '';
        if (!/^https?:\/\//i.test(value)) value = `https://${value}`;
        if (!value.endsWith('/')) value += '/';
        return value;
    }

    function bindGrafanaModeFields(container) {
        const mode = container.querySelector('#mcp-grafana-mode');
        if (!mode) return;
        const sync = () => {
            const isCloud = mode.value === 'cloud';
            const cloud = container.querySelector('#mcp-grafana-cloud-fields');
            const instance = container.querySelector('#mcp-grafana-instance-fields');
            if (cloud) cloud.style.display = isCloud ? '' : 'none';
            if (instance) instance.style.display = isCloud ? 'none' : '';
        };
        mode.onchange = sync;
        sync();
    }

    function renderConnectorFilters() {
        const bar = document.getElementById('connector-filters');
        if (!bar) return;
        const categories = visibleConnectorCategories();
        if (!categories.some((cat) => cat.id === connectorCategoryFilter)) {
            connectorCategoryFilter = 'all';
        }
        const searching = connectorSearchQuery.trim() !== '';
        const matchCounts = new Map();
        if (searching) {
            for (const id of listConnectorIds()) {
                if (!connectorMatchesSearch(id)) continue;
                const category = connectorCategory(id);
                matchCounts.set(category, (matchCounts.get(category) || 0) + 1);
                matchCounts.set('all', (matchCounts.get('all') || 0) + 1);
            }
        }
        bar.replaceChildren();
        for (const cat of categories) {
            const btn = document.createElement('button');
            btn.type = 'button';
            btn.className = 'connector-filter' + (connectorCategoryFilter === cat.id ? ' active' : '');
            btn.textContent = cat.label;
            btn.dataset.category = cat.id;
            if (searching) {
                const count = matchCounts.get(cat.id) || 0;
                const badge = document.createElement('span');
                badge.className = 'connector-filter__count';
                badge.textContent = String(count);
                btn.appendChild(badge);
                btn.classList.toggle('empty', count === 0 && connectorCategoryFilter !== cat.id);
            }
            btn.onclick = () => {
                if (connectorCategoryFilter === cat.id) return;
                connectorCategoryFilter = cat.id;
                renderConnectorFilters();
                renderConnectorList();
            };
            bar.appendChild(btn);
        }
    }

    function appendConnectorSection(list, title, ids, manifestById, modifier) {
        if (ids.length === 0) return;
        const section = document.createElement('section');
        section.className = 'connector-section' + (modifier ? ` connector-section--${modifier}` : '');
        if (title) {
            const heading = document.createElement('h3');
            heading.className = 'connector-section__title';
            heading.textContent = title;
            const count = document.createElement('span');
            count.className = 'connector-section__count';
            count.textContent = String(ids.length);
            heading.appendChild(count);
            section.appendChild(heading);
        }
        const grid = document.createElement('div');
        grid.className = 'connector-grid';
        for (const id of ids) grid.appendChild(buildConnectorRow(id, manifestById[id]));
        section.appendChild(grid);
        list.appendChild(section);
    }

    function renderConnectorList() {
        renderConnectorFilters();
        const list = document.getElementById('connector-list');
        list.replaceChildren();

        if (loadError) {
            const err = document.createElement('div');
            err.className = 'status-msg error';
            err.style.margin = '0 0 12px';
            err.textContent = loadError;
            list.appendChild(err);
        }

        if (!secureStorageAvailable) {
            const warn = document.createElement('div');
            warn.className = 'status-msg error';
            warn.style.margin = '0 0 12px';
            warn.textContent = 'Secure storage is unavailable on this system, so connectors that '
                + 'require saved credentials cannot connect. Install a keyring service '
                + '(for example gnome-keyring), then log out and back in.';
            list.appendChild(warn);
        }

        const manifestById = Object.fromEntries(manifests.map(m => [m.id, m]));
        const visibleIds = listConnectorIds().filter(
            (id) => connectorMatchesCategoryFilter(id) && connectorMatchesSearch(id)
        );
        const connectedIds = visibleIds.filter((id) => isConnected(manifestById[id]));
        const availableIds = visibleIds.filter((id) => !isConnected(manifestById[id]));

        if (connectorCategoryFilter === 'all') {
            appendConnectorSection(list, 'Connected', connectedIds, manifestById, 'connected');
            for (const cat of CONNECTOR_CATEGORIES) {
                if (cat.id === 'all') continue;
                const ids = availableIds.filter((id) => connectorCategory(id) === cat.id);
                appendConnectorSection(list, cat.label, ids, manifestById);
            }
        } else {
            appendConnectorSection(list, 'Connected', connectedIds, manifestById, 'connected');
            appendConnectorSection(list, connectedIds.length ? 'Available' : null, availableIds, manifestById);
        }

        if (visibleIds.length === 0) {
            const empty = document.createElement('div');
            empty.className = 'connector-list-empty';
            const query = connectorSearchQuery.trim();
            if (query) {
                const message = document.createElement('p');
                message.textContent = connectorCategoryFilter === 'all'
                    ? `No connectors match “${query}”.`
                    : `No connectors in this category match “${query}”.`;
                const addCustom = document.createElement('button');
                addCustom.type = 'button';
                addCustom.className = 'connector-list-empty__action';
                addCustom.textContent = 'Add it as a custom MCP server';
                addCustom.disabled = document.getElementById('add-mcp-btn').disabled;
                addCustom.onclick = () => openAddMcpModal();
                empty.append(message, addCustom);
            } else {
                empty.textContent = connectorCategoryFilter === 'all'
                    ? 'No connectors to show.'
                    : 'No connectors in this category.';
            }
            list.appendChild(empty);
        }
    }

    // Renders a bundled SVG when available, falling back to the emoji if the
    // asset is missing (e.g. a brand logo not yet dropped into webview/static/images/connectors/).
    function setIcon(el, cat) {
        el.textContent = '';
        if (cat && cat.iconSrc) {
            const img = document.createElement('img');
            img.className = 'connector-icon-img';
            img.alt = '';
            img.src = cat.iconSrc;
            img.onerror = () => { el.textContent = cat.icon || ''; };
            el.appendChild(img);
        } else {
            el.textContent = cat ? (cat.icon || '') : '';
        }
    }

    function buildConnectorRow(id, manifest) {
        const cat = getCatalogEntry(id);
        const connected = isConnected(manifest);
        const label = statusLabel(manifest);
        const functionsLabel = connectorFunctionsLabel(manifest);

        const row = document.createElement('div');
        row.className = 'connector-row' + (connected ? ' connected' : '');
        const body = document.createElement('div');
        body.className = 'connector-card-body';
        const iconSlot = document.createElement('div');
        iconSlot.className = 'connector-icon';
        const nameEl = document.createElement('div');
        nameEl.className = 'connector-name';
        nameEl.textContent = cat.name;
        const descEl = document.createElement('div');
        descEl.className = 'connector-desc';
        descEl.textContent = cat.description;
        body.append(iconSlot, nameEl, descEl);
        const footer = document.createElement('div');
        footer.className = 'connector-card-footer';
        const functionsEl = document.createElement('div');
        functionsEl.className = 'connector-functions';
        functionsEl.textContent = functionsLabel || '';
        const statusEl = document.createElement('div');
        statusEl.className = 'connector-status';
        statusEl.title = label;
        const dot = document.createElement('span');
        dot.className = statusDotClass(manifest);
        const statusText = document.createElement('span');
        statusText.className = 'connector-status-text';
        statusText.textContent = label;
        statusEl.append(dot, statusText);
        footer.append(functionsEl, statusEl);
        row.append(body, footer);
        setIcon(iconSlot, cat);
        row.onclick = () => openConnectorModal(id);
        return row;
    }

    async function loadAppSettings() {
        try {
            appSettings = await invoke('get_app_settings');
        } catch (e) {
            console.error('get_app_settings failed', e);
            appSettings = {};
        }
        filesystemRoots = [...(appSettings.filesystem_allowed_roots || [])];
        filesystemWriteEnabled = Boolean(appSettings.filesystem_write_enabled);
        bashEnabled = Boolean(appSettings.bash_enabled);
    }

    async function refreshData() {
        // A failing command must not abort init (which would leave the whole
        // page unresponsive). Degrade to empty + surface the error instead.
        loadError = null;
        await loadAppSettings();
        try {
            desktopFeatureFlags = await invoke('get_desktop_feature_flags');
        } catch (e) {
            desktopFeatureFlags = { experimentalConnectors: false };
            console.error('get_desktop_feature_flags failed', e);
        }
        try {
            manifests = await invoke('get_tool_registry');
        } catch (e) {
            manifests = [];
            loadError = `Failed to load connectors: ${e}`;
            console.error('get_tool_registry failed', e);
        }
        try {
            mcpServers = await invoke('list_mcp_servers');
        } catch (e) {
            mcpServers = [];
            loadError = `Failed to load MCP servers (a saved server config may be invalid): ${e}`;
            console.error('list_mcp_servers failed', e);
        }
        try {
            secureStorageAvailable = await invoke('is_secure_storage_available');
        } catch (e) {
            // Assume available: a failed check must not produce a banner claiming the
            // user's working keyring is broken.
            secureStorageAvailable = true;
            console.error('is_secure_storage_available failed', e);
        }
        renderConnectorList();
    }

    function openConnectorModal(id) {
        if (!listConnectorIds().includes(id)) return;
        activeConnectorId = id;
        const manifest = manifests.find(m => m.id === id);
        const cat = getCatalogEntry(id);
        const connected = isConnected(manifest);

        setIcon(document.getElementById('modal-icon'), cat);
        document.getElementById('modal-title').textContent = cat.name;
        document.getElementById('modal-reference').textContent = cat.reference;
        document.getElementById('modal-reference').closest('.field-row').classList.remove('hidden');
        document.getElementById('modal-description').textContent = cat.description;
        syncModalStatusFromManifest(manifest, id);
        document.querySelector('.privacy-note')?.classList.toggle('hidden', !connectorUsesKeychain(id));

        try {
            renderAuthFields(id, manifest);
            renderToolsList(id, manifest);
            updateModalActions(id, manifest, connected);
        } catch (e) {
            console.error('Failed to render connector modal', e);
            setModalMsg(`Failed to render this connector: ${e}`, 'error');
        }

        document.getElementById('refresh-tools-wrap').classList.toggle('hidden', !id.startsWith('mcp-'));

        document.querySelector('.modal-tabs').classList.remove('hidden');
        switchModalTab('overview');
        document.getElementById('connector-modal').classList.add('open');
    }

    function closeModal() {
        document.getElementById('connector-modal').classList.remove('open');
        activeConnectorId = null;
    }

    function switchModalTab(tab) {
        document.querySelectorAll('.modal-tab').forEach(t => {
            t.classList.toggle('active', t.dataset.modalTab === tab);
        });
        document.querySelectorAll('.modal-panel').forEach(p => p.classList.remove('active'));
        document.getElementById(`panel-${tab}`).classList.add('active');
    }

    function renderFilesystemAuth(container, manifest) {
        container.replaceChildren();
        const field = document.createElement('div');
        field.className = 'form-field';
        const label = document.createElement('label');
        label.textContent = 'Allowed folders';
        const list = document.createElement('ul');
        list.className = 'path-list';
        list.id = 'modal-path-list';
        const addBtn = document.createElement('button');
        addBtn.type = 'button';
        addBtn.className = 'ghost small';
        addBtn.id = 'modal-add-folder';
        addBtn.textContent = '+ Add folder';
        field.append(label, list, addBtn);
        container.append(
            field,
            checkboxGroup({
                id: 'modal-fs-write',
                checked: filesystemWriteEnabled,
                labelText: 'Allow the model to create and edit files in allowed folders',
                marginTop: '16px',
            })
        );
        if (filesystemRoots.length === 0) {
            const empty = document.createElement('li');
            empty.className = 'info';
            empty.textContent = 'No folders added yet.';
            list.appendChild(empty);
        } else {
            filesystemRoots.forEach((path, i) => {
                const li = document.createElement('li');
                const pathSpan = document.createElement('span');
                pathSpan.textContent = path;
                li.appendChild(pathSpan);
                const btn = document.createElement('button');
                btn.className = 'secondary small';
                btn.textContent = 'Remove';
                btn.onclick = async (e) => {
                    e.stopPropagation();
                    filesystemRoots.splice(i, 1);
                    renderAuthFields('filesystem', manifest);
                    renderConnectorList();
                    try {
                        await persistAppSettings({ refreshModal: true });
                    } catch (err) {
                        setModalMsg(String(err), 'error');
                    }
                };
                li.appendChild(btn);
                list.appendChild(li);
            });
        }
        container.querySelector('#modal-add-folder').onclick = async (e) => {
            e.stopPropagation();
            try {
                const path = await invoke('pick_directory');
                if (path && !filesystemRoots.includes(path)) {
                    filesystemRoots.push(path);
                    renderAuthFields('filesystem', manifest);
                    renderConnectorList();
                    await persistAppSettings({ refreshModal: true });
                }
            } catch (err) {
                setModalMsg(String(err), 'error');
            }
        };
        container.querySelector('#modal-fs-write').onchange = async (e) => {
            filesystemWriteEnabled = e.target.checked;
            renderConnectorList();
            try {
                await persistAppSettings({ refreshModal: true });
                setModalMsg(
                    filesystemWriteEnabled
                        ? 'File editing enabled — fs_write and fs_edit are now available.'
                        : 'File editing disabled.',
                    'success'
                );
            } catch (err) {
                setModalMsg(String(err), 'error');
            }
        };
    }

    function renderBashAuth(container) {
        container.replaceChildren();
        container.appendChild(infoParagraph(
            'Shell commands start in a folder configured under Filesystem. They are not sandboxed and can access files outside those folders with Lumo’s permissions.'
        ));
        container.appendChild(checkboxGroup({
            id: 'modal-bash-enabled',
            checked: bashEnabled,
            labelText: 'Allow the model to run shell commands',
            disabled: filesystemRoots.length === 0,
            marginTop: '12px',
        }));
        if (filesystemRoots.length === 0) {
            const hint = infoParagraph('Add at least one allowed folder under Filesystem first.');
            hint.style.marginTop = '12px';
            container.appendChild(hint);
        }
        const checkbox = container.querySelector('#modal-bash-enabled');
        if (checkbox) {
            checkbox.onchange = async (e) => {
                bashEnabled = e.target.checked;
                renderConnectorList();
                try {
                    await persistAppSettings({ refreshModal: true });
                    setModalMsg(
                        bashEnabled
                            ? 'Shell access enabled — bash__run is now available to the model.'
                            : 'Shell access disabled.',
                        'success'
                    );
                } catch (err) {
                    setModalMsg(String(err), 'error');
                }
            };
        }
    }

    function renderProtonMailAuth(container, config) {
        container.replaceChildren();
        const steps = document.createElement('div');
        steps.className = 'connector-setup-steps';
        const step1 = document.createElement('p');
        step1.className = 'info';
        const s1strong = document.createElement('strong');
        s1strong.textContent = '1.';
        step1.append(s1strong, ' Install and sign in to ');
        const bridgeLink = document.createElement('a');
        bridgeLink.href = 'https://proton.me/mail/bridge';
        bridgeLink.textContent = 'Proton Mail Bridge';
        step1.append(bridgeLink, ' (a paid Proton Mail plan is required).');
        const step2 = document.createElement('p');
        step2.className = 'info';
        const s2strong = document.createElement('strong');
        s2strong.textContent = '2.';
        const mailboxStrong = document.createElement('strong');
        mailboxStrong.textContent = 'Mailbox details';
        step2.append(
            s2strong,
            ' In Bridge, open your account\'s ',
            mailboxStrong,
            ' and copy the username, password, ports and security below. Use the Bridge password, not your Proton account password.'
        );
        steps.append(step1, step2);
        const hostInput = document.createElement('input');
        hostInput.type = 'text';
        hostInput.id = 'proton-host';
        hostInput.value = '127.0.0.1';
        hostInput.disabled = true;
        const usernameInput = document.createElement('input');
        usernameInput.type = 'text';
        usernameInput.id = 'proton-username';
        usernameInput.value = config.username || '';
        usernameInput.autocomplete = 'username';
        const passwordInput = document.createElement('input');
        passwordInput.type = 'password';
        passwordInput.id = 'proton-password';
        passwordInput.placeholder = config.username ? 'Stored — leave blank to keep' : 'Password shown by Bridge';
        passwordInput.autocomplete = 'current-password';
        const imapPort = document.createElement('input');
        imapPort.type = 'number';
        imapPort.id = 'proton-imap-port';
        imapPort.min = '1';
        imapPort.max = '65535';
        imapPort.value = String(config.imap_port || 1143);
        const smtpPort = document.createElement('input');
        smtpPort.type = 'number';
        smtpPort.id = 'proton-smtp-port';
        smtpPort.min = '1';
        smtpPort.max = '65535';
        smtpPort.value = String(config.smtp_port || 1025);
        const grid = document.createElement('div');
        grid.className = 'mail-settings-grid';
        grid.append(
            formFieldWithControl('IMAP port', imapPort),
            formFieldWithControl('IMAP security', securitySelect('proton-imap-security', config.imap_security || 'start_tls')),
            formFieldWithControl('SMTP port', smtpPort),
            formFieldWithControl('SMTP security', securitySelect('proton-smtp-security', config.smtp_security || 'tls'))
        );
        container.append(
            steps,
            formFieldWithControl('Bridge host', hostInput),
            formFieldWithControl('Bridge username', usernameInput),
            formFieldWithControl('Bridge password', passwordInput),
            grid,
            infoParagraph('Can\'t connect? Make sure Bridge is running and signed in, and that the ports and security match Mailbox details.')
        );
    }

    function renderLuminanceAuth(container, config) {
        container.replaceChildren();
        const stored = Boolean(config.instance_url);
        const instanceInput = document.createElement('input');
        instanceInput.type = 'text';
        instanceInput.id = 'luminance-instance';
        instanceInput.placeholder = 'acme or https://acme.app.luminance.com';
        instanceInput.value = config.instance_url || '';
        instanceInput.autocomplete = 'off';

        const authSelect = document.createElement('select');
        authSelect.id = 'luminance-auth';
        for (const [value, label] of [
            ['client_credentials', 'Client ID and Secret (renews automatically)'],
            ['access_token', 'Access token'],
        ]) {
            const option = document.createElement('option');
            option.value = value;
            option.textContent = label;
            option.selected = (config.auth || 'client_credentials') === value;
            authSelect.appendChild(option);
        }

        const keepHint = (method) => (stored && config.auth === method ? 'Stored — leave blank to keep' : '');
        const clientIdInput = document.createElement('input');
        clientIdInput.type = 'text';
        clientIdInput.id = 'luminance-client-id';
        clientIdInput.placeholder = keepHint('client_credentials') || 'Service user Client ID';
        clientIdInput.autocomplete = 'off';
        const clientSecretInput = document.createElement('input');
        clientSecretInput.type = 'password';
        clientSecretInput.id = 'luminance-client-secret';
        clientSecretInput.placeholder = keepHint('client_credentials') || 'Service user Client Secret';
        clientSecretInput.autocomplete = 'off';
        const clientFields = document.createElement('div');
        clientFields.append(
            formFieldWithControl('Client ID', clientIdInput),
            formFieldWithControl('Client Secret', clientSecretInput),
            infoParagraph('Lumo exchanges these for short-lived access tokens and renews them as needed.')
        );

        const tokenInput = document.createElement('input');
        tokenInput.type = 'password';
        tokenInput.id = 'luminance-access-token';
        tokenInput.placeholder = keepHint('access_token') || 'Access token';
        tokenInput.autocomplete = 'off';
        const tokenFields = document.createElement('div');
        tokenFields.append(
            formFieldWithControl('Access token', tokenInput),
            infoParagraph('Access tokens expire. When it does, paste a new one, or switch to Client ID and Secret.')
        );

        const syncAuth = () => {
            const clientCredentials = authSelect.value === 'client_credentials';
            clientFields.style.display = clientCredentials ? '' : 'none';
            tokenFields.style.display = clientCredentials ? 'none' : '';
        };
        authSelect.onchange = syncAuth;
        syncAuth();

        container.append(
            infoParagraph('Uses the Luminance Public API with a service user. Ask your Luminance admin for a Client ID and Secret.'),
            formFieldWithControl('Instance', instanceInput),
            formFieldWithControl('Sign in with', authSelect),
            clientFields,
            tokenFields,
            infoParagraph('Luminance allows 100 API requests every 10 minutes. Changes and deletions ask for approval by default.')
        );
    }

    function renderMcpStdioPresetAuth(container, preset) {
        container.replaceChildren(
            infoParagraph('Runs locally on this device — no credentials to enter.'),
            createFieldRow('Command', preset.command, 'field-value-mono'),
            createFieldRow('Args', (preset.args || []).join(' '), 'field-value-mono')
        );
    }

    function renderMcpByoAuth(container, preset, server) {
        container.replaceChildren();
        container.appendChild(infoParagraph(
            preset.byoHint || 'Connect your own workspace using your app\'s OAuth credentials.'
        ));
        appendPresetUrlField(container, preset, server);
        const clientId = document.createElement('input');
        clientId.type = 'text';
        clientId.id = 'mcp-client-id';
        clientId.value = server?.auth?.client_id || '';
        const clientSecret = document.createElement('input');
        clientSecret.type = 'password';
        clientSecret.id = 'mcp-client-secret';
        clientSecret.placeholder = server ? 'Stored — leave blank to keep' : '';
        container.append(
            formFieldWithControl('Client ID', clientId),
            formFieldWithControl('Client Secret', clientSecret)
        );
    }

    function renderMcpHeadersAuth(container, preset, server) {
        container.replaceChildren();
        container.appendChild(infoParagraph('Enter your credentials. Values are stored in your OS keychain.'));
        appendPresetUrlField(container, preset, server);
        for (const [i, header] of preset.headers.entries()) {
            const input = document.createElement('input');
            input.type = 'password';
            input.id = `mcp-header-${i}`;
            input.placeholder = server ? 'Stored — leave blank to keep' : '';
            container.appendChild(formFieldWithControl(header.label || header.name, input));
        }
    }

    function renderGrafanaMcpAuth(container, server, manifest) {
        container.replaceChildren();
        const mode = mcpGrafanaModeFromServer(server);
        const savedStack = mcpGrafanaStackFromServer(server);
        const savedUrl = server?.env?.GRAFANA_URL || '';
        const runner = mcpGrafanaRunnerFromServer(server);
        const skipTlsVerify = mcpGrafanaTlsSkipFromServer(server);
        const stored = manifest?.readiness?.state === 'ready';

        const modeSelect = document.createElement('select');
        modeSelect.id = 'mcp-grafana-mode';
        for (const [value, label] of [
            ['instance', 'Grafana instance (URL + service account)'],
            ['cloud', 'Grafana Cloud (OAuth)'],
        ]) {
            const option = document.createElement('option');
            option.value = value;
            option.textContent = label;
            option.selected = mode === value;
            modeSelect.appendChild(option);
        }
        container.appendChild(formFieldWithControl('Connection type', modeSelect));

        const instanceFields = document.createElement('div');
        instanceFields.id = 'mcp-grafana-instance-fields';
        const instanceInfo = document.createElement('p');
        instanceInfo.className = 'info';
        instanceInfo.append('Runs the ');
        const grafanaDoc = document.createElement('a');
        grafanaDoc.href = 'https://grafana.com/docs/grafana-cloud/ai-tools/mcp-servers/oss-mcp/';
        grafanaDoc.textContent = 'open source Grafana MCP server';
        instanceInfo.append(grafanaDoc, ' on this device. Use your Grafana root URL, including any subpath (e.g. ');
        const urlExample = document.createElement('code');
        urlExample.textContent = 'https://example.com/grafana/';
        instanceInfo.append(urlExample, ').');
        const runnerSelect = document.createElement('select');
        runnerSelect.id = 'mcp-grafana-runner';
        for (const [value, label] of [
            ['docker', 'Docker (grafana/mcp-grafana)'],
            ['uvx', 'uvx (requires uv)'],
        ]) {
            const option = document.createElement('option');
            option.value = value;
            option.textContent = label;
            option.selected = runner === value;
            runnerSelect.appendChild(option);
        }
        const grafanaUrl = document.createElement('input');
        grafanaUrl.type = 'text';
        grafanaUrl.id = 'mcp-grafana-url';
        grafanaUrl.placeholder = 'https://host/grafana/';
        grafanaUrl.value = savedUrl;
        const tokenInput = document.createElement('input');
        tokenInput.type = 'password';
        tokenInput.id = 'mcp-grafana-token';
        tokenInput.placeholder = stored ? 'Stored — leave blank to keep' : 'glsa_…';
        instanceFields.append(
            instanceInfo,
            formFieldWithControl('Launcher', runnerSelect),
            formFieldWithControl('Grafana URL', grafanaUrl),
            formFieldWithControl('Service account token', tokenInput),
            checkboxGroup({
                id: 'mcp-grafana-tls-skip',
                checked: skipTlsVerify,
                labelText: 'Skip TLS certificate verification (insecure — use only for self-signed or private CAs)',
                marginTop: '12px',
            })
        );

        const cloudFields = document.createElement('div');
        cloudFields.id = 'mcp-grafana-cloud-fields';
        cloudFields.appendChild(infoParagraph(
            'Sign in via OAuth in your browser. Optionally enter your stack name to skip the URL step during authorization.'
        ));
        const stackRow = document.createElement('div');
        stackRow.style.display = 'flex';
        stackRow.style.alignItems = 'center';
        stackRow.style.gap = '6px';
        const stackInput = document.createElement('input');
        stackInput.type = 'text';
        stackInput.id = 'mcp-grafana-stack';
        stackInput.placeholder = 'your-stack';
        stackInput.value = savedStack;
        stackInput.style.flex = '1';
        const stackSuffix = document.createElement('span');
        stackSuffix.textContent = '.grafana.net';
        stackRow.append(stackInput, stackSuffix);
        cloudFields.appendChild(formFieldWithControl('Stack name (optional)', stackRow));

        container.append(instanceFields, cloudFields);
        bindGrafanaModeFields(container);
    }

    function renderGrafanaMcpConnected(container, server) {
        container.replaceChildren();
        if (server?.transport === 'stdio') {
            container.appendChild(createFieldRow('URL', server.env?.GRAFANA_URL || '—', 'field-value-mono'));
            container.appendChild(infoParagraph('Connected with a service account token.'));
            if (mcpGrafanaTlsSkipFromServer(server)) {
                container.appendChild(infoParagraph('TLS certificate verification is disabled for this connection.'));
            }
        } else {
            container.appendChild(infoParagraph('Connected to Grafana Cloud via OAuth.'));
        }
    }

    function renderCustomMcpServerAuth(container, server, manifest) {
        container.replaceChildren();
        if (server?.transport === 'http') {
            container.appendChild(createFieldRow('URL', server.url || '—', 'field-value-mono'));
        } else {
            container.append(
                createFieldRow('Command', server?.command || '—', 'field-value-mono'),
                createFieldRow('Args', (server?.args || []).join(' ') || '—', 'field-value-mono')
            );
        }
        const secretEnv = secretEnvNames(server);
        const stored = manifest?.readiness?.state === 'ready';
        if (secretEnv.length > 0 && !isConnected(manifest)) {
            container.appendChild(infoParagraph(
                'This server reads its secrets from its environment. Values are stored in your OS keychain.'
            ));
            for (const [i, name] of secretEnv.entries()) {
                const input = document.createElement('input');
                input.type = 'password';
                input.id = `mcp-env-${i}`;
                input.placeholder = stored ? 'Stored — leave blank to keep' : '';
                container.appendChild(formFieldWithControl(name, input));
            }
        }
    }

    function mountAddMcpAuthFields(container) {
        container.replaceChildren();
        const idInput = document.createElement('input');
        idInput.type = 'text';
        idInput.id = 'add-mcp-id';
        idInput.placeholder = 'notion';
        const nameInput = document.createElement('input');
        nameInput.type = 'text';
        nameInput.id = 'add-mcp-name';
        nameInput.placeholder = 'Notion';
        const descriptionInput = document.createElement('textarea');
        descriptionInput.id = 'add-mcp-description';
        descriptionInput.rows = 2;
        descriptionInput.placeholder = 'Search and read notes in your workspace.';
        const transportSelect = document.createElement('select');
        transportSelect.id = 'add-mcp-transport';
        for (const [value, label] of [
            ['stdio', 'Local (command)'],
            ['http', 'Remote (HTTP)'],
        ]) {
            const option = document.createElement('option');
            option.value = value;
            option.textContent = label;
            transportSelect.appendChild(option);
        }
        const stdioFields = document.createElement('div');
        stdioFields.id = 'add-mcp-stdio-fields';
        const commandInput = document.createElement('input');
        commandInput.type = 'text';
        commandInput.id = 'add-mcp-command';
        commandInput.placeholder = 'npx';
        const argsInput = document.createElement('textarea');
        argsInput.id = 'add-mcp-args';
        argsInput.rows = 2;
        argsInput.placeholder = '-y\n@notionhq/notion-mcp-server';
        const envArea = document.createElement('textarea');
        envArea.id = 'add-mcp-env';
        envArea.rows = 2;
        envArea.placeholder = 'GITHUB_HOST=github.com';
        const secretEnvArea = document.createElement('textarea');
        secretEnvArea.id = 'add-mcp-secret-env';
        secretEnvArea.rows = 2;
        secretEnvArea.placeholder = 'GITHUB_PERSONAL_ACCESS_TOKEN=ghp_…';
        stdioFields.append(
            formFieldWithControl('Args (one per line)', argsInput),
            formGrid(
                formFieldWithControl('Environment', envArea),
                formFieldWithControl('Secret environment', secretEnvArea)
            ),
            fieldHint('One KEY=VALUE per line. Environment is saved in plain text; secret values go to your OS keychain.')
        );

        const httpFields = document.createElement('div');
        httpFields.id = 'add-mcp-http-fields';
        httpFields.style.display = 'none';
        const urlInput = document.createElement('input');
        urlInput.type = 'text';
        urlInput.id = 'add-mcp-url';
        urlInput.placeholder = 'https://mcp.example.com/mcp';
        const authTypeSelect = document.createElement('select');
        authTypeSelect.id = 'add-mcp-auth-type';
        for (const [value, label] of [
            ['oauth', 'OAuth'],
            ['token', 'Token (static header)'],
            ['none', 'None (public server)'],
        ]) {
            const option = document.createElement('option');
            option.value = value;
            option.textContent = label;
            authTypeSelect.appendChild(option);
        }
        const oauthFields = document.createElement('details');
        oauthFields.id = 'add-mcp-oauth-fields';
        oauthFields.className = 'form-advanced';
        const oauthSummary = document.createElement('summary');
        oauthSummary.textContent = 'Advanced OAuth settings';
        const scopesInput = document.createElement('input');
        scopesInput.type = 'text';
        scopesInput.id = 'add-mcp-scopes';
        const oauthClientId = document.createElement('input');
        oauthClientId.type = 'text';
        oauthClientId.id = 'add-mcp-client-id';
        oauthClientId.placeholder = 'leave blank to auto-register';
        const oauthClientSecret = document.createElement('input');
        oauthClientSecret.type = 'password';
        oauthClientSecret.id = 'add-mcp-client-secret';
        scopesInput.placeholder = 'comma-separated';
        oauthFields.append(
            oauthSummary,
            formFieldWithControl('Scopes', scopesInput),
            formGrid(
                formFieldWithControl('Client ID', oauthClientId),
                formFieldWithControl('Client secret', oauthClientSecret)
            )
        );
        const tokenFields = document.createElement('div');
        tokenFields.id = 'add-mcp-token-fields';
        tokenFields.style.display = 'none';
        const schemeSelect = document.createElement('select');
        schemeSelect.id = 'add-mcp-token-scheme';
        for (const scheme of ['Bearer', 'Basic']) {
            const option = document.createElement('option');
            option.value = scheme;
            option.textContent = scheme;
            schemeSelect.appendChild(option);
        }
        const tokenInput = document.createElement('input');
        tokenInput.type = 'password';
        tokenInput.id = 'add-mcp-token';
        const tokenHint = fieldHint('Sent as ');
        const code = document.createElement('code');
        code.textContent = 'Authorization: <scheme> <token>';
        tokenHint.append(code, '. For Basic, enter base64(user:token).');
        const tokenGrid = formGrid(
            formFieldWithControl('Scheme', schemeSelect),
            formFieldWithControl('Token', tokenInput)
        );
        tokenGrid.classList.add('form-grid-narrow-first');
        tokenFields.append(tokenGrid, tokenHint);
        httpFields.append(
            formFieldWithControl('URL', urlInput),
            oauthFields,
            tokenFields
        );

        const descriptionField = formFieldWithControl('Description (optional)', descriptionInput);
        descriptionField.append(fieldHint('Shared with Lumo so it can suggest this server before you connect. Don\'t include credentials.'));
        const authField = formFieldWithControl('Auth', authTypeSelect);
        authField.style.display = 'none';
        const commandField = formFieldWithControl('Command', commandInput);

        container.append(
            formGrid(
                formFieldWithControl('ID', idInput),
                formFieldWithControl('Display name', nameInput)
            ),
            descriptionField,
            formGrid(
                formFieldWithControl('Transport', transportSelect),
                commandField,
                authField
            ),
            stdioFields,
            httpFields
        );

        const privacyNote = document.querySelector('.privacy-note');
        const syncVisibility = () => {
            const isHttp = transportSelect.value === 'http';
            const authType = authTypeSelect.value;
            stdioFields.style.display = isHttp ? 'none' : '';
            httpFields.style.display = isHttp ? '' : 'none';
            authField.style.display = isHttp ? '' : 'none';
            commandField.style.display = isHttp ? 'none' : '';
            oauthFields.style.display = authType === 'oauth' ? '' : 'none';
            tokenFields.style.display = authType === 'token' ? '' : 'none';
            privacyNote?.classList.toggle('hidden', isHttp && authType === 'none');
        };
        transportSelect.onchange = syncVisibility;
        authTypeSelect.onchange = syncVisibility;
    }

    function renderAuthFields(id, manifest) {
        const container = document.getElementById('modal-auth-fields');
        container.replaceChildren();

        if (id === 'filesystem') {
            renderFilesystemAuth(container, manifest);
        } else if (id === 'bash') {
            renderBashAuth(container);
        } else if (id === 'proton_mail') {
            renderProtonMailAuth(container, manifest?.auth?.config || {});
        } else if (id === 'luminance') {
            renderLuminanceAuth(container, manifest?.auth?.config || {});
        } else if (id.startsWith('mcp-')) {
            const preset = MCP_CATALOG[id];
            const server = mcpServers.find(s => `mcp-${s.id}` === id);
            if (preset && preset.transport === 'stdio' && preset.subdomain && !isConnected(manifest)) {
                renderZendeskMcpAuth(container, {
                    savedSubdomain: mcpSubdomainFromZendeskServer(server),
                    preset,
                    connected: false,
                });
            } else if (preset && preset.transport === 'stdio' && preset.subdomain && isConnected(manifest)) {
                renderZendeskMcpAuth(container, {
                    subdomain: mcpSubdomainFromZendeskServer(server),
                    preset,
                    connected: true,
                });
            } else if (preset && preset.transport === 'stdio') {
                renderMcpStdioPresetAuth(container, preset);
            } else if (id === 'mcp-figma') {
                renderFigmaMcpAuth(container, { preset, connected: isConnected(manifest) });
            } else if (preset && preset.auth === 'none' && preset.url) {
                renderPublicMcpAuth(container, preset, isConnected(manifest));
            } else if (preset && preset.auth === 'byo' && !isConnected(manifest)) {
                renderMcpByoAuth(container, preset, server);
            } else if (preset && preset.auth === 'headers' && !isConnected(manifest)) {
                renderMcpHeadersAuth(container, preset, server);
            } else if (preset && preset.grafana && !isConnected(manifest)) {
                renderGrafanaMcpAuth(container, server, manifest);
            } else if (preset && preset.grafana && isConnected(manifest)) {
                renderGrafanaMcpConnected(container, server);
            } else if (preset && !isConnected(manifest)) {
                container.appendChild(infoParagraph('Sign in via OAuth in your browser — no credentials to enter.'));
                appendPresetUrlField(container, preset, server);
            } else if (preset) {
                container.appendChild(infoParagraph('Connected.'));
            } else {
                renderCustomMcpServerAuth(container, server, manifest);
            }
        }
    }

    function prettifyToolName(rawName) {
        const segment = rawName.split('__').pop();
        return segment
            .split('_')
            .filter(Boolean)
            .map((word) => word.charAt(0).toUpperCase() + word.slice(1))
            .join(' ');
    }

    function renderToolsList(id, manifest) {
        const container = document.getElementById('modal-tools-list');
        const tools = manifest?.tools || [];
        document.getElementById('tools-section-title').textContent =
            tools.length ? `Tools ${tools.length}` : 'No tools available';

        container.replaceChildren();
        if (tools.length === 0) {
            container.appendChild(infoParagraph('Connect this integration to discover available tools.'));
            return;
        }

        if (!isConnected(manifest)) {
            const note = document.createElement('p');
            note.className = 'info';
            note.textContent = 'Finish setup above — these tools are only offered to the model once this connector is connected.';
            container.appendChild(note);
        }

        for (const tool of tools) {
            const row = document.createElement('div');
            row.className = 'tool-row';

            const info = document.createElement('div');
            info.className = 'tool-info';
            const name = document.createElement('div');
            name.className = 'tool-name';
            name.textContent = tool.title || prettifyToolName(tool.name);
            info.append(name);
            const desc = document.createElement('div');
            desc.className = 'tool-desc';
            desc.textContent = tool.description || '';
            info.append(desc);
            if (tool.description) {
                desc.classList.add('clamped');
                desc.addEventListener('click', () => desc.classList.toggle('clamped'));
            }

            row.append(info, buildPolicyControl(id, tool, manifest));
            container.appendChild(row);
        }
    }

    // Approval-policy values must match the Rust `ToolPolicy` serde (snake_case), except
    // `always`, which is auto-approve plus `always_allow_tool`.
    const TOOL_POLICIES = [
        { value: 'auto_approve', label: '✓', title: 'Auto-approve, but ask after reading emails, web pages or other untrusted content' },
        { value: 'always', label: '!', title: 'Always allow, even after reading untrusted content and in scheduled tasks' },
        { value: 'ask', label: '?', title: 'Ask before each run' },
        { value: 'disable', label: '✗', title: 'Disabled' },
    ];

    const ALWAYS_ALLOW_WARNING =
        'Always allow this tool?\n\nEmails, web pages and other outside content can contain hidden instructions. ' +
        'With this on, Lumo will use this tool without asking even after reading such content, including in ' +
        'scheduled tasks, so those instructions could make it send your data elsewhere or change your files.';

    function buildPolicyControl(id, tool, manifest) {
        const seg = document.createElement('div');
        seg.className = 'policy-seg';
        const current = () =>
            (manifest?.always_allowed || []).includes(tool.name) ? 'always' : tool.policy || 'auto_approve';
        for (const opt of TOOL_POLICIES) {
            const btn = document.createElement('button');
            btn.type = 'button';
            btn.textContent = opt.label;
            btn.title = opt.title;
            btn.dataset.policy = opt.value;
            btn.classList.toggle('active', current() === opt.value);
            btn.onclick = async (e) => {
                e.stopPropagation();
                if (current() === opt.value) return;
                if (opt.value === 'always' && !confirm(ALWAYS_ALLOW_WARNING)) return;
                try {
                    if (opt.value === 'always') {
                        await invoke('always_allow_tool', { connectorId: id, toolName: tool.name });
                    } else {
                        await invoke('set_tool_policy', {
                            connectorId: id,
                            toolName: tool.name,
                            policy: opt.value,
                        });
                    }
                    // Optimistically sync in-memory state so re-renders stay correct
                    // without a full get_tool_registry round-trip (avoids MCP churn).
                    tool.policy = opt.value === 'always' ? 'auto_approve' : opt.value;
                    if (manifest) {
                        const others = (manifest.always_allowed || []).filter((name) => name !== tool.name);
                        manifest.always_allowed = opt.value === 'always' ? [...others, tool.name] : others;
                    }
                    seg.querySelectorAll('button').forEach((b) =>
                        b.classList.toggle('active', b.dataset.policy === opt.value)
                    );
                } catch (err) {
                    setModalMsg(`Failed to update policy: ${err}`, 'error');
                }
            };
            seg.appendChild(btn);
        }
        return seg;
    }

    /** Offered where a connector can hold credentials and currently does. */
    function canForgetCredentials(manifest) {
        return Boolean(manifest?.capabilities?.credentials) && manifest?.readiness?.state === 'ready';
    }

    /** Clears a catalog MCP preset's saved config (and keychain secrets). */
    function canResetMcpPreset(id, server, preset) {
        return id.startsWith('mcp-') && Boolean(server) && Boolean(preset);
    }

    function updateModalActions(id, manifest, connected) {
        const primary = document.getElementById('modal-primary');
        const disconnect = document.getElementById('modal-disconnect');
        const remove = document.getElementById('modal-remove');

        const pending = pendingConnections.has(id);
        disconnect.disabled = remove.disabled = document.getElementById('refresh-tools').disabled = pending;
        if (pending) {
            primary.disabled = true;
            showConnectionCancel(id);
            return;
        }
        primary.disabled = false;
        disconnect.textContent = 'Disconnect';
        disconnect.classList.toggle('hidden', !canForgetCredentials(manifest));
        remove.classList.add('hidden');
        remove.onclick = null;

        if (id === 'filesystem') {
            primary.textContent = 'Done';
            primary.onclick = () => { closeModal(); };
        } else if (id === 'bash') {
            primary.textContent = bashEnabled ? 'Enabled' : 'Enable shell access';
            primary.disabled = filesystemRoots.length === 0;
            primary.onclick = async () => {
                if (filesystemRoots.length === 0) return;
                bashEnabled = true;
                const checkbox = document.getElementById('modal-bash-enabled');
                if (checkbox) checkbox.checked = true;
                renderConnectorList();
                try {
                    await persistAppSettings({ refreshModal: true });
                    closeModal();
                } catch (err) {
                    setModalMsg(String(err), 'error');
                }
            };
        } else if (id === 'proton_mail') {
            primary.textContent = connected ? 'Save and test' : 'Connect';
            primary.onclick = async () => {
                const username = document.getElementById('proton-username')?.value.trim();
                const password = document.getElementById('proton-password')?.value || '';
                const imapPort = Number(document.getElementById('proton-imap-port')?.value);
                const smtpPort = Number(document.getElementById('proton-smtp-port')?.value);
                if (!username || !Number.isInteger(imapPort) || !Number.isInteger(smtpPort)
                    || imapPort < 1 || imapPort > 65535 || smtpPort < 1 || smtpPort > 65535) {
                    setModalMsg('Enter a username and valid IMAP and SMTP ports.', 'error');
                    return;
                }
                try {
                    setModalMsg('Testing IMAP and SMTP connections…');
                    await invoke('connect_connector', {
                        connectorId: id,
                        credentials: {
                            username,
                            password,
                            imap_port: imapPort,
                            smtp_port: smtpPort,
                            imap_security: document.getElementById('proton-imap-security')?.value,
                            smtp_security: document.getElementById('proton-smtp-security')?.value,
                        },
                    });
                    setModalMsg('Connected.', 'success');
                    await refreshData();
                    openConnectorModal(id);
                } catch (e) {
                    setModalMsg(String(e), 'error');
                }
            };
            disconnect.onclick = async () => {
                if (!confirm('Disconnect Proton Mail and delete its saved Bridge settings and password?')) return;
                try {
                    await invoke('disconnect_connector', { connectorId: id });
                    await refreshData();
                    openConnectorModal(id);
                } catch (e) {
                    setModalMsg(String(e), 'error');
                }
            };
        } else if (id === 'luminance') {
            primary.textContent = connected ? 'Save and test' : 'Connect';
            primary.onclick = async () => {
                const instance = document.getElementById('luminance-instance')?.value.trim();
                if (!instance) {
                    setModalMsg('Enter your Luminance instance.', 'error');
                    return;
                }
                try {
                    setModalMsg('Checking your Luminance credentials…');
                    await invoke('connect_connector', {
                        connectorId: id,
                        credentials: {
                            instance,
                            auth: document.getElementById('luminance-auth')?.value,
                            client_id: document.getElementById('luminance-client-id')?.value || '',
                            client_secret: document.getElementById('luminance-client-secret')?.value || '',
                            access_token: document.getElementById('luminance-access-token')?.value || '',
                        },
                    });
                    setModalMsg('Connected.', 'success');
                    await refreshData();
                    openConnectorModal(id);
                } catch (e) {
                    setModalMsg(String(e), 'error');
                }
            };
            disconnect.onclick = async () => {
                if (!confirm('Disconnect Luminance and delete its saved instance and credentials?')) return;
                try {
                    await invoke('disconnect_connector', { connectorId: id });
                    await refreshData();
                    openConnectorModal(id);
                } catch (e) {
                    setModalMsg(String(e), 'error');
                }
            };
        } else if (id.startsWith('mcp-')) {
            const preset = MCP_CATALOG[id];
            const server = mcpServers.find(s => `mcp-${s.id}` === id);
            primary.textContent = connected ? 'Connected' : 'Connect';
            primary.disabled = connected;
            const resetPreset = canResetMcpPreset(id, server, preset);
            const removable = Boolean(server) && !preset;
            const showRemove = removable || resetPreset;
            remove.classList.toggle('hidden', !showRemove);
            remove.textContent = resetPreset ? 'Reset' : 'Remove';
            if (showRemove) {
                remove.onclick = async () => {
                    const name = getCatalogEntry(id).name;
                    const message = resetPreset
                        ? `Reset ${name}? This removes the saved server configuration and any stored OAuth tokens so you can connect again from scratch.`
                        : `Remove ${name}? This disconnects it and deletes stored credentials.`;
                    if (!confirm(message)) return;
                    try {
                        await invoke('remove_mcp_server', { serverId: server.id, featured: Boolean(preset) });
                        closeModal();
                        await refreshData();
                    } catch (e) {
                        setModalMsg(String(e), 'error');
                    }
                };
            }
            disconnect.classList.toggle(
                'hidden',
                !canForgetCredentials(manifest) || resetPreset
            );
            primary.onclick = () => withConnectionAttempt(id, async () => {
                const credentials = { headers: {} };
                try {
                    // Featured presets persist their config (from the frontend
                    // catalog) before authorizing; custom servers are already saved.
                    if (preset && preset.auth === 'none' && preset.url && !connected) {
                        await invoke('save_mcp_server', { featured: true,
                            server: {
                                id: preset.server_id,
                                display_name: preset.name,
                                transport: 'http',
                                url: preset.url,
                                auth: { type: 'none' },
                            },
                        });
                    } else if (preset && preset.transport === 'stdio' && !connected) {
                        const env = {};
                        if (preset.subdomain) {
                            const subdomain = document.getElementById('mcp-subdomain')?.value.trim();
                            if (!subdomain) {
                                setModalMsg('Subdomain is required.', 'error');
                                return;
                            }
                            env.ZENDESK_SUBDOMAIN = subdomain;
                        }
                        await invoke('save_mcp_server', { featured: true,
                            server: {
                                id: preset.server_id,
                                display_name: preset.name,
                                transport: 'stdio',
                                command: preset.command,
                                args: preset.args,
                                env,
                            },
                        });
                    } else if (preset && preset.auth === 'headers' && !connected) {
                        const values = preset.headers.map((h, i) =>
                            document.getElementById(`mcp-header-${i}`)?.value.trim() || ''
                        );
                        const existing = mcpServers.find(s => s.id === preset.server_id);
                        if (!existing && values.some(v => !v)) {
                            setModalMsg('All credentials are required.', 'error');
                            return;
                        }
                        const url = readPresetUrl(preset);
                        if (preset.urlInput && !url) return;
                        const headers = preset.headers.map(h => ({ name: h.name, prefix: h.prefix || '' }));
                        const server = { id: preset.server_id, display_name: preset.name, transport: 'http', url, auth: { type: 'headers', headers } };
                        if (preset.request_headers) server.request_headers = preset.request_headers;
                        await invoke('save_mcp_server', { featured: true, server });
                        preset.headers.forEach((h, i) => {
                            if (values[i]) credentials.headers[h.name] = values[i];
                        });
                    } else if (!preset && secretEnvNames(server).length && !connected) {
                        const names = secretEnvNames(server);
                        const values = names.map((name, i) =>
                            document.getElementById(`mcp-env-${i}`)?.value.trim() || ''
                        );
                        const stored = manifest?.readiness?.state === 'ready';
                        if (!stored && values.some(v => !v)) {
                            setModalMsg('Every secret environment variable needs a value.', 'error');
                            return;
                        }
                        credentials.env = {};
                        names.forEach((name, i) => {
                            if (values[i]) credentials.env[name] = values[i];
                        });
                    } else if (preset && preset.grafana && !connected) {
                        const mode = document.getElementById('mcp-grafana-mode')?.value;
                        if (mode === 'instance') {
                            const grafanaUrl = normalizeGrafanaUrl(document.getElementById('mcp-grafana-url')?.value);
                            const token = document.getElementById('mcp-grafana-token')?.value.trim() || '';
                            const existing = mcpServers.find(s => s.id === preset.server_id);
                            if (!grafanaUrl) {
                                setModalMsg('Grafana URL is required.', 'error');
                                return;
                            }
                            if (!existing && !token) {
                                setModalMsg('Service account token is required.', 'error');
                                return;
                            }
                            const runner = document.getElementById('mcp-grafana-runner')?.value || 'docker';
                            const skipTlsVerify = document.getElementById('mcp-grafana-tls-skip')?.checked;
                            await invoke('save_mcp_server', { featured: true,
                                server: grafanaInstanceServerConfig(preset, grafanaUrl, runner, skipTlsVerify),
                            });
                            if (token) credentials.env = { GRAFANA_SERVICE_ACCOUNT_TOKEN: token };
                        } else {
                            const auth = { type: 'oauth', scopes: preset.scopes };
                            if (preset.resource) auth.resource = preset.resource;
                            if (preset.authorize_params) auth.authorize_params = preset.authorize_params;
                            const server = {
                                id: preset.server_id,
                                display_name: preset.name,
                                transport: 'http',
                                url: preset.url,
                                auth,
                            };
                            const headers = mcpGrafanaRequestHeaders(
                                document.getElementById('mcp-grafana-stack')?.value
                            );
                            if (headers) server.request_headers = headers;
                            if (preset.client_id) auth.client_id = preset.client_id;
                            await invoke('save_mcp_server', { featured: true, server });
                            if (preset.auth === 'confidential' && preset.client_secret) {
                                credentials.client_secret = preset.client_secret;
                            }
                        }
                    } else if (preset && !connected) {
                        const auth = { type: 'oauth', scopes: preset.scopes };
                        if (preset.resource) auth.resource = preset.resource;
                        if (preset.authorize_params) auth.authorize_params = preset.authorize_params;
                        let url = readPresetUrl(preset);
                        if (preset.urlInput && !url) return;
                        if (preset.subdomain) {
                            const subdomain = document.getElementById('mcp-subdomain')?.value.trim();
                            if (!subdomain) {
                                setModalMsg('Subdomain is required.', 'error');
                                return;
                            }
                            url = mcpPresetUrl(preset, subdomain);
                        }
                        const server = {
                            id: preset.server_id,
                            display_name: preset.name,
                            transport: 'http',
                            url,
                            auth,
                        };
                        if (preset.auth === 'byo') {
                            const clientId = document.getElementById('mcp-client-id')?.value.trim();
                            const secret = document.getElementById('mcp-client-secret')?.value.trim();
                            if (!clientId) { setModalMsg('Client ID is required.', 'error'); return; }
                            auth.client_id = clientId;
                            await invoke('save_mcp_server', { featured: true, server });
                            if (secret) credentials.client_secret = secret;
                        } else {
                            if (preset.client_id) auth.client_id = preset.client_id;
                            await invoke('save_mcp_server', { featured: true, server });
                            if (preset.auth === 'confidential' && preset.client_secret) {
                                credentials.client_secret = preset.client_secret;
                            }
                        }
                    }
                    const grafanaCloudOAuth = preset?.grafana
                        && document.getElementById('mcp-grafana-mode')?.value === 'cloud';
                    const opensBrowser = preset
                        ? (grafanaCloudOAuth || (!preset.grafana && preset.auth !== 'headers' && preset.auth !== 'none'))
                        : server?.transport === 'http' && server?.auth?.type === 'oauth';
                    setModalMsg(
                        preset?.connectHint
                            ?? (opensBrowser ? 'Opening browser to authorize…' : 'Connecting…')
                    );
                    await invoke('connect_connector', { connectorId: id, credentials });
                    setModalMsg('Connected.', 'success');
                    await refreshData();
                    openConnectorModal(id);
                } catch (e) {
                    setModalMsg(String(e), 'error');
                }
            });
            disconnect.onclick = async () => {
                if (!confirm(`Disconnect ${getCatalogEntry(id).name} and delete its stored credentials?`)) return;
                try {
                    await invoke('disconnect_connector', { connectorId: id });
                    await refreshData();
                    openConnectorModal(id);
                } catch (e) {
                    setModalMsg(String(e), 'error');
                }
            };
        }
    }

    function setModalMsg(msg, type = '') {
        const el = document.getElementById('modal-status-msg');
        if (activeConnectorId) {
            if (type === 'error') connectionErrors.set(activeConnectorId, msg);
            else connectionErrors.delete(activeConnectorId);
        }
        if (type === 'error' && activeConnectorId) {
            renderStatusMessage(el, activeConnectorId, msg, 'error');
        } else {
            el.textContent = msg;
            el.className = `status-msg ${type}`.trim();
        }
    }

    function openAddMcpModal() {
        activeConnectorId = '__add_mcp__';
        document.querySelector('.privacy-note')?.classList.remove('hidden');
        setIcon(document.getElementById('modal-icon'), { icon: '🔗', iconSrc: 'images/connectors/mcp.svg' });
        document.getElementById('modal-title').textContent = 'Add MCP server';
        document.getElementById('modal-reference').closest('.field-row').classList.add('hidden');
        document.getElementById('modal-description').textContent = 'Register a Model Context Protocol server. Tools are discovered when you connect.';
        document.getElementById('modal-status-dot').className = 'status-dot';
        document.getElementById('modal-status-text').textContent = 'Not configured';
        mountAddMcpAuthFields(document.getElementById('modal-auth-fields'));
        document.getElementById('modal-tools-list').replaceChildren();
        document.getElementById('tools-section-title').textContent = '';
        document.getElementById('refresh-tools-wrap').classList.add('hidden');
        document.getElementById('modal-disconnect').classList.add('hidden');
        document.getElementById('modal-remove').classList.add('hidden');
        document.getElementById('modal-primary').textContent = 'Add server';
        document.getElementById('modal-primary').disabled = false;
        document.getElementById('modal-primary').onclick = async () => {
            const id = document.getElementById('add-mcp-id').value.trim();
            const display_name = document.getElementById('add-mcp-name').value.trim();
            const description = document.getElementById('add-mcp-description').value.trim() || undefined;
            const transport = document.getElementById('add-mcp-transport').value;
            if (!id || !display_name) {
                setModalMsg('ID and name are required.', 'error');
                return;
            }
            return withConnectionAttempt(`mcp-${id}`, async () => {
            const credentials = { headers: {} };
            try {
                if (transport === 'stdio') {
                    const command = document.getElementById('add-mcp-command').value.trim();
                    const argsRaw = document.getElementById('add-mcp-args').value.trim();
                    const args = argsRaw ? argsRaw.split('\n').map(s => s.trim()).filter(Boolean) : [];
                    if (!command) { setModalMsg('Command is required.', 'error'); return; }
                    const env = parseEnvLines(document.getElementById('add-mcp-env').value);
                    const secretValues = parseEnvLines(document.getElementById('add-mcp-secret-env').value);
                    if (!env || !secretValues) {
                        setModalMsg('Environment lines must be in KEY=VALUE form.', 'error');
                        return;
                    }
                    const secret_env = Object.keys(secretValues);
                    if (secret_env.some(name => !secretValues[name])) {
                        setModalMsg('Every secret environment variable needs a value.', 'error');
                        return;
                    }
                    // A declared secret must not also be persisted in the plaintext env map.
                    for (const name of secret_env) delete env[name];
                    await invoke('save_mcp_server', { server: { id, display_name, description, transport: 'stdio', command, args, env, secret_env } });
                    credentials.env = secretValues;
                } else {
                    const url = document.getElementById('add-mcp-url').value.trim();
                    if (!url) { setModalMsg('URL is required.', 'error'); return; }
                    const authType = document.getElementById('add-mcp-auth-type').value;
                    if (authType === 'token') {
                        const scheme = document.getElementById('add-mcp-token-scheme').value;
                        const token = document.getElementById('add-mcp-token').value.trim();
                        if (!token) { setModalMsg('Token is required.', 'error'); return; }
                        const server = { id, display_name, description, transport: 'http', url, auth: { type: 'token', scheme } };
                        await invoke('save_mcp_server', { server });
                        credentials.token = token;
                    } else if (authType === 'none') {
                        const server = { id, display_name, description, transport: 'http', url, auth: { type: 'none' } };
                        await invoke('save_mcp_server', { server });
                    } else {
                        const scopesRaw = document.getElementById('add-mcp-scopes').value.trim();
                        const scopes = scopesRaw ? scopesRaw.split(',').map(s => s.trim()).filter(Boolean) : [];
                        const clientId = document.getElementById('add-mcp-client-id').value.trim();
                        const secret = document.getElementById('add-mcp-client-secret').value.trim();
                        const auth = { type: 'oauth', scopes };
                        if (clientId) auth.client_id = clientId;
                        const server = { id, display_name, description, transport: 'http', url, auth };
                        await invoke('save_mcp_server', { server });
                        if (clientId && secret) credentials.client_secret = secret;
                    }
                }
                setModalMsg('Connecting…');
                await invoke('connect_connector', { connectorId: `mcp-${id}`, credentials });
                closeModal();
                await refreshData();
            } catch (e) {
                setModalMsg(String(e), 'error');
                await refreshData();
            }
            });
        };
        document.querySelector('.modal-tabs').classList.add('hidden');
        switchModalTab('overview');
        document.getElementById('connector-modal').classList.add('open');
    }

    function selectTab(tab) {
        const nav = document.querySelector(`.nav-item[data-tab="${tab}"]`);
        const panel = document.getElementById(`tab-${tab}`);
        if (!nav || !panel || nav.disabled) {
            return;
        }
        document.querySelectorAll('.nav-item').forEach(t => t.classList.remove('active'));
        document.querySelectorAll('.tab-panel').forEach(p => p.classList.remove('active'));
        nav.classList.add('active');
        panel.classList.add('active');
    }

    // A target is a tab id, or `connectors/<connector id>` to open that connector's details.
    function showSettingsTarget(target) {
        if (typeof target !== 'string') return;
        if (!target.startsWith(SETTINGS_CONNECTOR_TAB_PREFIX)) {
            selectTab(target);
            return;
        }
        selectTab('connectors');
        const id = target.slice(SETTINGS_CONNECTOR_TAB_PREFIX.length);
        if (listConnectorIds().includes(id)) {
            openConnectorModal(id);
        }
    }

    async function applySignedInState() {
        let signedIn = true;
        let resetAllowed = false;
        try {
            signedIn = await invoke('is_account_signed_in');
            resetAllowed = signedIn === true;
        } catch (e) {
            // Defaults to signed in so a failed check cannot hide a working page.
            console.error('is_account_signed_in failed', e);
        }

        const nav = document.querySelector('.nav-item[data-tab="connectors"]');
        nav.disabled = !signedIn;
        nav.title = signedIn ? '' : 'Sign in to configure connectors';
        document.getElementById('connectors-signed-out').classList.toggle('hidden', signedIn);
        document.getElementById('connectors-custom-mcp').classList.toggle('hidden', !signedIn);
        document.getElementById('connector-list').classList.toggle('hidden', !signedIn);
        document.getElementById('connector-search-bar').classList.toggle('hidden', !signedIn);
        document.getElementById('add-mcp-btn').disabled = !signedIn;
        const resetButton = document.getElementById('reset-configuration');
        resetButton.disabled = resetInProgress || !resetAllowed;
        resetButton.title = resetAllowed ? '' : 'Sign in to reset account configuration';

        if (!signedIn && nav.classList.contains('active')) {
            selectTab('general');
        }
        return signedIn;
    }

    document.addEventListener('DOMContentLoaded', async () => {
        // Keep recovery available even if configuration loading stalls.
        const resetButton = document.getElementById('reset-configuration');
        resetButton.onclick = async () => {
            if (resetButton.disabled || resetInProgress) return;
            resetInProgress = true;
            resetButton.disabled = true;
            try {
                await invoke('reset_local_configuration');
            } catch (e) {
                alert('Failed to reset local configuration: ' + e);
            } finally {
                resetInProgress = false;
                await applySignedInState();
            }
        };

        const signedIn = await applySignedInState();
        await loadAppSettings();

        document.getElementById('current-shortcut').textContent = appSettings.search_shortcut || '';
        document.getElementById('launch-startup').checked = Boolean(appSettings.launch_at_startup);
        document.getElementById('settings-hotkey').checked = appSettings.settings_hotkey_enabled !== false;
        document.getElementById('search-shortcut-enabled').checked = appSettings.search_shortcut_enabled !== false;
        if (signedIn) {
            await refreshData();
        }

        document.querySelectorAll('.nav-item').forEach(tab => {
            tab.addEventListener('click', () => selectTab(tab.dataset.tab));
        });

        // A fresh page load takes the tab the open request left behind; an already
        // loaded page is told over the event instead.
        try {
            const pendingTab = await invoke('take_pending_settings_tab');
            if (pendingTab) {
                showSettingsTarget(pendingTab);
            }
        } catch (e) {
            console.error('take_pending_settings_tab failed', e);
        }

        window.__TAURI__?.event?.listen('settings:show-tab', (event) => showSettingsTarget(event.payload));

        // Emitted on every account change, so signing out with this page open re-gates it.
        window.__TAURI__?.event?.listen('connectors:changed', async () => {
            if (await applySignedInState()) {
                await refreshData();
            }
        });

        // Emitted when a tool call in the Lumo window moves a connector's health.
        window.__TAURI__?.event?.listen('connector:changed', async () => {
            await refreshData();
            refreshActiveModal();
        });

        document.querySelectorAll('.modal-tab').forEach(tab => {
            tab.addEventListener('click', () => switchModalTab(tab.dataset.modalTab));
        });

        document.getElementById('add-mcp-btn').onclick = () => openAddMcpModal();

        const connectorSearch = document.getElementById('connector-search');
        connectorSearch.addEventListener('input', () => {
            const wasSearching = connectorSearchQuery.trim() !== '';
            connectorSearchQuery = connectorSearch.value;
            if (!wasSearching && connectorSearchQuery.trim() !== '') connectorCategoryFilter = 'all';
            renderConnectorList();
        });
        connectorSearch.addEventListener('keydown', (e) => {
            if (e.key !== 'Escape') return;
            // The page-level Escape handler closes the settings window.
            e.preventDefault();
            e.stopPropagation();
            if (connectorSearch.value) {
                connectorSearch.value = '';
                connectorSearchQuery = '';
                renderConnectorList();
            } else {
                connectorSearch.blur();
            }
        });
        document.addEventListener('keydown', (e) => {
            if (e.key !== '/' || e.metaKey || e.ctrlKey || e.altKey || recordingShortcut) return;
            if (!document.getElementById('tab-connectors').classList.contains('active')) return;
            if (document.getElementById('connector-modal').classList.contains('open')) return;
            const target = e.target;
            if (target instanceof HTMLElement
                && (target.isContentEditable || ['INPUT', 'TEXTAREA', 'SELECT'].includes(target.tagName))) return;
            e.preventDefault();
            connectorSearch.focus();
            connectorSearch.select();
        });

        document.getElementById('modal-close').onclick = closeModal;
        document.getElementById('modal-cancel').onclick = closeModal;

        document.getElementById('copy-reference').onclick = () => {
            navigator.clipboard.writeText(document.getElementById('modal-reference').textContent);
        };

        document.getElementById('refresh-tools').onclick = async () => {
            if (!activeConnectorId?.startsWith('mcp-')) return;
            const id = activeConnectorId;
            return withConnectionAttempt(id, async () => {
            setModalMsg('Refreshing tools…');
            try {
                await invoke('connect_connector', { connectorId: id, credentials: null });
                await refreshData();
                if (activeConnectorId === id) openConnectorModal(id);
                setModalMsg('Tools refreshed.', 'success');
            } catch (e) {
                setModalMsg(String(e), 'error');
            }
            });
        };

        const shortcutInput = document.getElementById('search-shortcut');
        let valueBeforeRecording = '';
        shortcutInput.addEventListener('click', () => {
            if (recordingShortcut) return;
            valueBeforeRecording = shortcutInput.value;
            shortcutInput.value = 'Press keys…';
            recordingShortcut = true;
        });
        document.addEventListener('keydown', (e) => {
            if (!recordingShortcut) return;
            e.preventDefault();
            e.stopImmediatePropagation();
            if (e.code === 'Escape') {
                shortcutInput.value = valueBeforeRecording;
                recordingShortcut = false;
                return;
            }
            if (MODIFIER_CODES.test(e.code)) {
                shortcutInput.value = pendingShortcutLabel(e);
                return;
            }
            shortcutInput.value = [...shortcutModifiers(e), shortcutKeyName(e.code)].join('+');
            recordingShortcut = false;
        });
        document.addEventListener('keyup', (e) => {
            if (!recordingShortcut || !MODIFIER_CODES.test(e.code)) return;
            shortcutInput.value = pendingShortcutLabel(e);
        });
        const shortcutEnabled = document.getElementById('search-shortcut-enabled');
        const syncShortcutEnabled = () => {
            shortcutInput.disabled = !shortcutEnabled.checked;
            document.getElementById('reset-shortcut').disabled = !shortcutEnabled.checked;
            if (!shortcutEnabled.checked && recordingShortcut) {
                shortcutInput.value = valueBeforeRecording;
                recordingShortcut = false;
            }
        };
        shortcutEnabled.addEventListener('change', syncShortcutEnabled);
        syncShortcutEnabled();
        document.getElementById('reset-shortcut').onclick = () => {
            shortcutInput.value = '';
            recordingShortcut = false;
        };

        document.getElementById('save-settings').onclick = async () => {
            try {
                await persistAppSettings();
                await invoke('close_settings_window');
            } catch (e) {
                alert('Failed to save: ' + e);
            }
        };

        const goBack = () => {
            shortcutInput.value = '';
            recordingShortcut = false;
            return invoke('close_settings_window');
        };
        document.getElementById('back-btn').onclick = goBack;
        document.addEventListener('keydown', (e) => {
            if (e.key !== 'Escape' || recordingShortcut) return;
            const modal = document.getElementById('connector-modal');
            if (modal.classList.contains('open')) {
                closeModal();
                return;
            }
            goBack();
        });
    });
