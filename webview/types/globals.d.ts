import type { AccountIdentity, DesktopExternalTools, LumoConnectorsApi } from '.';
import type { LumoSchedulesApi } from './schedules';
import type { SkillRuntime } from './skills';
import type { LumoNotificationsApi } from './notifications';
import type { LumoQuickAskApi } from './quick-ask';

declare global {
  type TauriInvokeArgs = Record<string, unknown>;

  type TauriInvoke = <T = unknown>(command: string, args?: TauriInvokeArgs) => Promise<T>;

  type TauriUnlisten = () => void;

  type TauriListen = (
    event: string,
    handler: (event: { payload?: unknown }) => void
  ) => Promise<TauriUnlisten>;

  /**
   * Tauri IPC globals. `__TAURI_INTERNALS__` is absent outside a Tauri webview, and `__TAURI__`
   * exists only because `withGlobalTauri` is set in tauri.conf.json. Every member is optional, so
   * callers must fall back from the first to the second and handle neither being present.
   */
  interface Window {
    __TAURI_INTERNALS__?: {
      invoke?: TauriInvoke;
      event?: {
        listen?: TauriListen;
      };
    };
    __TAURI__?: {
      core?: {
        invoke?: TauriInvoke;
      };
      event?: {
        listen?: TauriListen;
      };
      external_tools?: DesktopExternalTools;
    };
    LumoConnectors?: LumoConnectorsApi;
    LumoSchedules?: LumoSchedulesApi;
    LumoNotifications?: LumoNotificationsApi;
    __lumoDownloadFeedback?: (result: { id: string | null; name: string | null; success: boolean; mac: boolean }) => void;
    LumoSkillRuntime?: SkillRuntime;
    LumoQuickAsk?: LumoQuickAskApi;
    /** Set by the quick-ask window; the shell calls it each time it shows that window. */
    __lumoRevealSearch?: () => void;
    __lumoAccountIdentity?: AccountIdentity;
    /** Gateway base path override for the browser build. */
    LUMO_CONNECTOR_API?: string;
  }
}
