/**
 * Shapes crossing the connector boundary.
 *
 * Mirrors `ConnectorManifest`, `ToolDefinition`, `Readiness`, `Activity`, `ConnectorStatus`,
 * `AuthMechanism` and `ToolPolicy` in
 * src-tauri/src/tools/. Nothing verifies the two stay in step, so a field renamed in Rust
 * shows up as a runtime failure, not a type error.
 */
import type { ConnectorCatalogEntry, ConnectorCategory, ConnectorCategoryId } from '../shared/connector-catalog';

/** `Readiness`, internally tagged on `state`. Whether a call is worth attempting. */
export type Readiness =
  | { state: 'ready' }
  | { state: 'blocked'; reason: string };

/** `Activity`, internally tagged on `state`. How the last attempt to reach the remote went. */
export type Activity =
  | { state: 'untried' }
  | { state: 'succeeded' }
  | { state: 'failed'; error: string };

/** `ConnectorStatus`, internally tagged on `state`. The judgement the connector list renders. */
export type ConnectorStatus =
  | { state: 'ready' }
  | { state: 'warning'; error: string }
  | { state: 'not_ready'; reason: string };

/** `Capabilities`. Fixed at construction, so it gates which buttons a card offers. */
export type Capabilities = {
  credentials: boolean;
  session: boolean;
};

/** `ToolPolicy`, serialized `snake_case`. */
export type ToolPolicy = 'auto_approve' | 'ask' | 'disable';

/** `AuthMechanism`, internally tagged on `type`. Only the tag is modelled; the bridge reads nothing else. */
export type AuthMechanism = {
  type: 'none' | 'api_key' | 'bearer_token' | 'oauth2' | 'mcp_server';
  [key: string]: unknown;
};

export type ToolDefinition = {
  name: string;
  title?: string;
  description?: string;
  input_schema?: unknown;
  policy?: ToolPolicy;
  /** Unknown when absent. Only explicit true is eligible for read-only scheduled access. */
  read_only?: boolean;
};

/**
 * A connector as it arrives from either producer.
 *
 * Rust sends every field. The Lumo gateway and the built-in browser manifest omit the ones the
 * bridge never reads, which is why those are optional here rather than following Rust exactly.
 */
export type ConnectorManifest = {
  id: string;
  display_name: string;
  description?: string;
  readiness: Readiness;
  status: ConnectorStatus;
  version?: string;
  icon?: string | null;
  auth?: AuthMechanism;
  capabilities?: Capabilities;
  tools?: ToolDefinition[];
  /** Tools the user lets run even after the conversation has read untrusted content. */
  always_allowed?: string[];
  enabled?: boolean;
  /** Absent when the connector has no remote, so there is no such thing as a failure for it. */
  activity?: Activity | null;
  /** Added by the bridge from the connector catalog, not sent by Rust. */
  category?: ConnectorCategoryId;
  /** Added by the bridge for MCP connectors: the id without its `mcp-` prefix. */
  server_id?: string;
  /** Added by the bridge for HTTP MCP connectors. */
  url?: string;
};

/** Public recommendation metadata. Tool metadata is descriptive, not an execution grant. */
export type CatalogConnector = Omit<ConnectorCatalogEntry, 'iconSrc' | 'icon'> & {
  /** Bundled image data URI, usable from the embedded remote page. */
  icon?: string;
  registered: boolean;
  /** Absent for presets that have not been registered. */
  enabled?: boolean;
  readiness?: Readiness;
  status?: ConnectorStatus;
  /** Discovered/cached tools only; an empty list does not imply no capabilities. */
  tools: Pick<ToolDefinition, 'name' | 'title' | 'description' | 'read_only'>[];
};

/** Presets plus registered connectors. Never carries auth config or tool input schemas. */
export type ConnectorCatalog = {
  categories: ConnectorCategory[];
  connectors: CatalogConnector[];
};

/** A tool renamed to `<connector>__<tool>` and tagged with its origin. */
export type NamespacedTool = ToolDefinition & {
  connector_id: string;
  tool_name: string;
};

export type JsonSchemaObject = {
  type?: string;
  $schema?: string;
  properties?: Record<string, unknown>;
  required?: string[];
  [key: string]: unknown;
};

export type OpenAITool = {
  type: 'function';
  function: {
    name: string;
    description: string;
    parameters: JsonSchemaObject;
  };
};

/** @deprecated Flat Lumo shape. */
export type FlatApiTool = {
  name: string;
  description: string;
  parameters: JsonSchemaObject;
};

/** An OpenAI tool call, accepted in Chat Completions or flat form. */
export type ToolCall = {
  id?: string | null;
  tool_call_id?: string | null;
  name?: string;
  arguments?: unknown;
  function?: {
    name?: string;
    description?: string;
    arguments?: unknown;
  };
};

export type NormalizedToolCall = {
  id: string | null;
  name: string;
  arguments: Record<string, unknown>;
};

export type ToolResultMessage = {
  role: 'tool';
  tool_call_id: string | null;
  name: string;
  content: string;
  fileTransfer?: unknown;
  is_error?: boolean;
};

export type ToolApprovalRequestPayload = {
  request_id?: string;
  connector_id?: string;
  tool_name?: string;
  input?: unknown;
  reason?: ToolApprovalReason | null;
  /** The conversation the web app named on the tool call, if any. */
  conversation_id?: string | null;
};

/** Why native asks: the conversation has read untrusted content and the call can send data out or change files. */
export type ToolApprovalReason = {
  /**
   * `untrusted_write`: the call changes files rather than sending data out.
   * `hidden_text`: the call would send invisible characters, shown in its input as `[U+XXXX]`.
   */
  kind: 'untrusted_content' | 'untrusted_write' | 'hidden_text';
  /**
   * `service`: what a connected service such as Slack or Jira returned.
   * `connector`: tool descriptions from an MCP server the user added themselves.
   */
  sources: ('mail' | 'web' | 'shell' | 'service' | 'connector')[];
  destination?: string;
};

/** Per-call context the web app supplies alongside a tool call. */
export type ToolCallOptions = {
  /** Lets native track untrusted content per conversation rather than per session. */
  conversationId?: string;
};

export type ToolApprovalResolvedPayload = {
  request_id?: string;
  approved?: boolean;
  conversation_id?: string | null;
};

export type ScheduleToolContext = { connectionId: string; runId: string };
export type ScheduleExecutionInput = {
  runId: string;
  connectorIds: string[];
  autoApprove: boolean;
  /** Subset of connectorIds. Omission preserves unrestricted connector access. */
  readOnlyConnectorIds?: string[];
};
export type ScheduleExecutionCapabilities = { readOnlyConnectors: boolean };

export type SettingsTab = 'general' | 'connectors';

export type BrowserFetchInput = {
  url: string;
  format?: 'text' | 'html' | 'json';
};

export type BrowserFetchResult = {
  url: string;
  status: number;
  content_type: string;
  format: string;
  content: unknown;
};

export type ToolsChangedDetail = {
  manifests: ConnectorManifest[];
  nativeTools: NamespacedTool[];
  availableTools: NamespacedTool[];
  openaiTools: OpenAITool[];
};

export type PdfExportOptions = {
  /** Suggested name in the Save dialog; `.pdf` is added when missing. */
  fileName: string;
  /** Print job title; defaults to the file name. */
  title?: string;
};

/** `cancelled`: the user dismissed the Save dialog. `failed`: rendering or saving failed. */
export type PdfExportResult = { status: 'saved' | 'cancelled' | 'failed' };

/** `window.LumoConnectors`, consumed by the settings UI and the Lumo web app. */
export type LumoConnectorsApi = {
  ready: Promise<void>;
  init: () => Promise<void>;
  loadRegistry: () => Promise<ConnectorManifest[]>;
  setFeatureFlags: (flags: { experimentalConnectors: boolean }) => Promise<void>;
  getNativeTools: () => NamespacedTool[];
  getAvailableTools: () => NamespacedTool[];
  getManifests: () => ConnectorManifest[];
  getConnectors: () => ConnectorManifest[];
  setConnectorEnabled: (connectorId: string, enabled: boolean) => Promise<void>;
  setToolPolicy: (connectorId: string, toolName: string, policy: ToolPolicy) => Promise<void>;
  /** `always`: auto-approve this tool from now on; untrusted content still makes it ask. */
  respondToolApproval: (requestId: string, approved: boolean, always?: boolean) => Promise<void>;
  getFlatApiTools: () => FlatApiTool[];
  getOpenAITools: () => OpenAITool[];
  toFlatApiTool: (tool: ToolDefinition) => FlatApiTool;
  toFlatApiTools: (tools: ToolDefinition[]) => FlatApiTool[];
  toOpenAITool: (tool: ToolDefinition) => OpenAITool;
  toOpenAITools: (tools: ToolDefinition[]) => OpenAITool[];
  ensureOpenAIToolShape: (tool: OpenAITool | FlatApiTool) => OpenAITool;
  normalizeParameters: (schema: unknown) => JsonSchemaObject;
  isNativeTool: (name: string) => boolean;
  isAvailableTool: (name: string) => boolean;
  dispatchNativeTool: (namespacedName: string, input: unknown, scheduleContext?: ScheduleToolContext) => Promise<unknown>;
  executeToolCall: (toolCall: ToolCall, scheduleContext?: ScheduleToolContext, options?: ToolCallOptions) => Promise<ToolResultMessage>;
  executeToolCalls: (toolCalls: ToolCall[], scheduleContext?: ScheduleToolContext, options?: ToolCallOptions) => Promise<ToolResultMessage[]>;
  scheduleExecutionCapabilities: ScheduleExecutionCapabilities;
  registerScheduleExecutionContext: (input: ScheduleExecutionInput) => Promise<ScheduleToolContext>;
  connectConnector: (connectorId: string, credentials?: unknown) => Promise<unknown>;
  disconnectConnector: (connectorId: string) => Promise<unknown>;
  /** With a connector id, opens the connectors tab on that connector's details. */
  openSettings: (tab?: SettingsTab, connectorId?: string) => Promise<void>;
  getCatalog: () => ConnectorCatalog;
  /**
   * Renders a document's HTML to a real PDF with the OS print engine, after a native Save dialog.
   * Present only where the platform supports it (macOS for now); callers feature-detect it and
   * fall back otherwise.
   */
  exportPdf?: (html: string, options: PdfExportOptions) => Promise<PdfExportResult>;
  isDesktop: boolean;
  isBrowser: boolean;
};

/**
 * `window.__TAURI__.external_tools`, where LumoAPIClient discovers native tools.
 *
 * Clients declares its own view of this in `core/desktop-tools.ts`. The two are not checked against
 * each other; see the dropped task 4 in docs/plan-typescript-migration.md.
 */
export type DesktopExternalTools = {
  completeFileRead?: (resultId: string, value: unknown) => Promise<unknown>;
  ready: Promise<void>;
  getTools: () => OpenAITool[];
  getOpenAITools: () => OpenAITool[];
  getFlatApiTools: () => FlatApiTool[];
  getAvailableTools: () => NamespacedTool[];
  isNativeTool: (name: string) => boolean;
  executeToolCall: (toolCall: ToolCall, scheduleContext?: ScheduleToolContext, options?: ToolCallOptions) => Promise<ToolResultMessage>;
  executeToolCalls: (toolCalls: ToolCall[], scheduleContext?: ScheduleToolContext, options?: ToolCallOptions) => Promise<ToolResultMessage[]>;
  scheduleExecutionCapabilities: ScheduleExecutionCapabilities;
  registerScheduleExecutionContext: (input: ScheduleExecutionInput) => Promise<ScheduleToolContext>;
};

/**
 * `window.__lumoAccountIdentity`, installed by account-identity.js.
 *
 * `resolve` always returns an object; it reports `userId: null` rather than nothing when no signal
 * identifies a session. Callers rely on that, so the return is not nullable.
 */
export type AccountIdentity = {
  resolve?: (pathname?: string) => AccountResolution;
};

/** Which signal identified the session slot, and the UserID found in it. */
export type AccountResolution = {
  userId: string | null;
  signal: 'path' | 'llid' | 'single-session' | 'none';
  slot: number | null;
};
