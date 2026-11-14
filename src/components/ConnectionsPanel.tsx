import { useEffect, useId, useRef, useState, type ReactNode, type Ref } from "react";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { invoke } from "@tauri-apps/api/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { errorKind, failureText, saveErrorText } from "../present";
import { groupConnections, hostOf, keyFormOpen, monogram, servicesFor, type Service, type ServiceId } from "../services";
import type { Connection, ProviderKind, SaveError, Settings } from "../settings";
import { Card, focusWithin, IconButton, isComposing, Modal, Row, rowAfterRemoval, useConfirmKey, useFocusAfter, useFocusAfterBusy, usePressGuard } from "../ui";
import chatgptLogo from "../assets/chatgpt-logo-white.svg";
import { mark, type LogoId } from "../logos";

/// A service's mark in the round badge, in the text color; letters for an own server.
function ServiceMark({ logo, letters }: { logo?: LogoId; letters: string }) {
  if (!logo) {
    return (
      <span className="monogram" aria-hidden="true">
        {letters}
      </span>
    );
  }
  const { viewBox, paths, evenOdd } = mark(logo);
  return (
    <span className="monogram service-mark" aria-hidden="true">
      <svg viewBox={viewBox} width="16" height="16" fill="currentColor" fillRule={evenOdd ? "evenodd" : undefined}>
        {paths.map((d) => (
          <path key={d.slice(0, 24)} d={d} />
        ))}
      </svg>
    </span>
  );
}

/// Opens the ChatGPT usage settings in the browser; a browser that does not open shows nothing
/// to fix here.
export function openChatGptUsage() {
  void invoke("open_chatgpt_usage").catch(() => {});
}

/// Mirrors `ConnectionError` in `commands.rs`; `settings` carries a `SaveError`.
type ConnectionError = { kind: "settings"; detail: SaveError } | { kind: string; detail?: string };

/// Mirrors `Removed` in `commands.rs`.
type Removed = { settings: Settings; sign_out_unconfirmed: boolean };

/// A `ConnectionError` by its kind; anything else, such as a command that did not run, as it is.
export function connectionErrorText(t: TFunction, failure: unknown): string {
  const error = failure as Partial<ConnectionError> | null;
  // `settings`: the change was not saved.
  if (typeof error === "object" && error !== null && typeof error.detail === "object" && error.detail !== null) {
    return saveErrorText(t, error.detail);
  }
  return failureText(t, failure, "connections.error", "connections.error.other");
}

/// How long the button that replaces "Continue with ChatGPT" lets presses go: the second click
/// of a double click must not cancel the sign-in it started.
const CANCEL_GUARD_MS = 500;

/// The first button of a row's controls: where the focus goes once the row's key form closed or
/// its key was removed ("Add key", "Refresh", "Remove").
const firstControl = (row: HTMLElement | null | undefined) => row?.querySelector<HTMLElement>(".card-row-control button");

/// The role of a row's message: a refusal is said at once, a result when the reader is free.
const messageRole = (error: boolean) => (error ? "alert" : "status");

interface Props {
  settings: Settings;
  onSettings: (settings: Settings) => void;
}

/// "Services": named services with their keys and own servers with an OpenAI-style API. A key
/// is sent once, checked by listing the service's models, stored encrypted and never shown
/// again. `active`: the page is the one shown.
export function ConnectionsPanel({ settings, onSettings, active }: Props & { active: boolean }) {
  const { t } = useTranslation();
  return (
    <>
      <p className="caption">{t("connections.note")}</p>
      <Card title={t("connections.section_speech")}>
        <ServiceList settings={settings} onSettings={onSettings} only="speech" />
      </Card>
      <Card title={t("connections.section_text")}>
        <ChatGptRow settings={settings} onSettings={onSettings} active={active} />
        <ServiceList settings={settings} onSettings={onSettings} only="text" continued />
      </Card>
      <OwnServers settings={settings} onSettings={onSettings} />
    </>
  );
}

/// ChatGPT through the user's account, first in the text services as the recommended way: modes
/// then use their Plus or Pro plan instead of a key. Signing in happens in the browser; OpenAI
/// asks there whether ahem may use the plan. The welcome after the first sign-in waits until the
/// page is shown (`active`).
function ChatGptRow({ settings, onSettings, active }: Props & { active: boolean }) {
  const { t } = useTranslation();
  const { account } = groupConnections(settings.connections);
  const actions = useConnections(onSettings);
  const [waiting, setWaiting] = useState(false);
  const [welcome, setWelcome] = useState(false);
  const cancelGuard = usePressGuard();
  // The row's buttons take each other's place: "Cancel" while signing in, the account's buttons
  // after it; a refresh disables them for a moment. The focus goes to the first button there.
  const follow = useFocusAfter();
  const row = useRef<HTMLDivElement>(null);
  const toRow = () => follow(() => firstControl(row.current));

  async function connect() {
    const first = account === undefined;
    cancelGuard.hold(CANCEL_GUARD_MS);
    toRow();
    setWaiting(true);
    // Cancelling here or declining in the browser is a change of mind, not a problem.
    const connected = await actions.run("chatgpt", () => invoke<Settings>("connect_chatgpt"), undefined, [
      "sign_in_cancelled",
      "sign_in_denied",
    ]);
    // "Cancel" goes as the sign-in ends. A focus the user took elsewhere during the long wait (a
    // key's field) is not this row's to move.
    if (focusWithin(row.current, document.activeElement)) toRow();
    setWaiting(false);
    // Shown once, when the plan is first used: later sign-ins are the same account.
    if (connected && first) setWelcome(true);
  }

  // The welcome opened as "Cancel" went: the focus it had before is gone too.
  function closeWelcome() {
    toRow();
    setWelcome(false);
  }

  const message = actions.message?.id === "chatgpt" || actions.message?.id === account?.id ? actions.message : null;
  return (
    <>
      <div ref={row} className="card-row service-row">
        <div className="card-row-main">
          <ServiceMark logo="chatgpt" letters="CG" />
          <div className="card-row-text">
            <span className="service-title">
              <span className="strong">{t("chatgpt.title")}</span>
              <span className="cap-chip recommended">{t("chatgpt.recommended")}</span>
            </span>
            {account ? (
              <span className="key-meta">
                {account.name} · {t("chatgpt.connected")} · {t("connections.model_count", { number: account.models.length })}
              </span>
            ) : (
              // The account leaves the settings at once; OpenAI ends its session after that.
              <span className="caption">
                {waiting ? t("chatgpt.waiting") : actions.disconnecting !== null ? t("chatgpt.disconnecting") : t("chatgpt.note")}
              </span>
            )}
          </div>
          <div className="card-row-control">
            {waiting ? (
              <button
                type="button"
                className="btn compact"
                onClick={() => {
                  if (cancelGuard.passes()) void invoke("cancel_chatgpt_sign_in").catch(() => {});
                }}
              >
                {t("connections.cancel")}
              </button>
            ) : account ? (
              <>
                <button type="button" className="btn link" onClick={openChatGptUsage}>
                  {t("chatgpt.manage_usage")}
                </button>
                <button
                  type="button"
                  className="btn link"
                  disabled={actions.busy !== null}
                  onClick={() => {
                    toRow();
                    void actions.refresh(account);
                  }}
                >
                  {actions.busy === account.id ? t("connections.refreshing") : t("connections.refresh")}
                </button>
                <button type="button" className="btn link" disabled={actions.busy !== null} onClick={connect}>
                  {t("chatgpt.sign_in_again")}
                </button>
                <button
                  type="button"
                  className="btn link quiet"
                  disabled={actions.busy !== null}
                  onClick={() => actions.remove(account, toRow)}
                >
                  {actions.disconnecting === account.id
                    ? t("chatgpt.disconnecting")
                    : actions.removing === account.id
                      ? t("chatgpt.confirm_disconnect")
                      : t("chatgpt.disconnect")}
                </button>
              </>
            ) : (
              <button type="button" className="chatgpt-button" data-oai-siwc-button disabled={actions.busy !== null} onClick={connect}>
                <img src={chatgptLogo} alt="" width={18} height={18} />
                {t("chatgpt.continue")}
              </button>
            )}
          </div>
        </div>
        {message && (
          <p className={message.error ? "caption warning-text service-status" : "caption service-status"} role={messageRole(message.error)}>
            {message.text}
          </p>
        )}
      </div>
      {welcome && active && (
        <Modal labelledBy="chatgpt-welcome" onClose={closeWelcome}>
          <h3 id="chatgpt-welcome" className="card-title">{t("chatgpt.welcome_title")}</h3>
          <p className="caption">{t("chatgpt.welcome_text")}</p>
          <div className="modal-actions">
            <button type="button" className="btn link" onClick={openChatGptUsage}>
              {t("chatgpt.manage_usage")}
            </button>
            <button type="button" className="btn primary" autoFocus onClick={closeWelcome}>
              {t("chatgpt.got_it")}
            </button>
          </div>
        </Modal>
      )}
    </>
  );
}

/// Adding, refreshing and removing connections, with one row busy at a time.
function useConnections(onSettings: (settings: Settings) => void) {
  const { t } = useTranslation();
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{ id: string; text: string; error: boolean } | null>(null);
  // A key is removed by a second press on its row.
  const [removing, confirmRemove] = useConfirmKey<string>();
  // A ChatGPT account waits for OpenAI to end its session.
  const [disconnecting, setDisconnecting] = useState<string | null>(null);

  /// `quiet`: failures that are the user's own choice and need no message.
  async function run(
    id: string,
    command: () => Promise<Settings>,
    done?: string,
    quiet: string[] = [],
  ): Promise<boolean> {
    setBusy(id);
    setMessage(null);
    try {
      onSettings(await command());
      if (done) setMessage({ id, text: done, error: false });
      return true;
    } catch (failure) {
      if (!quiet.includes(errorKind(failure) ?? "other")) setMessage({ id, text: connectionErrorText(t, failure), error: true });
      return false;
    } finally {
      setBusy(null);
    }
  }

  const add = (id: string, args: { provider: ProviderKind; name: string; baseUrl: string | null; apiKey: string }) =>
    run(id, () => invoke<Settings>("add_connection", args), t("connections.added_ok"));

  const refresh = (connection: Connection) =>
    run(connection.id, () => invoke<Settings>("refresh_connection_models", { id: connection.id }));

  /// `leave`: called as the removal begins, which takes away the button pressed.
  function remove(connection: Connection, leave?: () => void) {
    confirmRemove(connection.id, () => {
      leave?.();
      void disconnect(connection);
    });
  }

  async function disconnect(connection: Connection) {
    setDisconnecting(connection.id);
    let unconfirmed = false;
    const removed = await run(connection.id, async () => {
      const result = await invoke<Removed>("remove_connection", { id: connection.id });
      unconfirmed = result.sign_out_unconfirmed;
      return result.settings;
    });
    setDisconnecting(null);
    // Only a ChatGPT account signs out; the message stays in its row once the account is gone.
    if (removed && unconfirmed) setMessage({ id: "chatgpt", text: t("chatgpt.sign_out_unconfirmed"), error: true });
  }

  /// Checks and stores the key of a named service; `false` if it was not added.
  async function addKey(service: Service, key: string): Promise<boolean> {
    if (!key.trim()) {
      setMessage({ id: service.id, text: t("connections.error.missing_key"), error: true });
      return false;
    }
    const name = t(`connections.services.${service.id}`);
    return add(service.id, { provider: service.provider, name, baseUrl: service.baseUrl, apiKey: key });
  }

  /// Puts a new key in place of a connection's (an expired one), keeping the connection, so the
  /// modes that use it go on; `false` if it was refused, and the old key stays.
  function replaceKey(connection: Connection, key: string): Promise<boolean> {
    if (!key.trim()) {
      setMessage({ id: connection.id, text: t("connections.error.missing_key"), error: true });
      return Promise.resolve(false);
    }
    return run(
      connection.id,
      () => invoke<Settings>("replace_connection_key", { id: connection.id, apiKey: key }),
      t("connections.replaced_ok"),
    );
  }

  return { busy, message, setMessage, removing, disconnecting, run, add, addKey, replaceKey, refresh, remove };
}

/// The named services, one row each; `only` keeps the services that recognize speech or those
/// with language models. A service doing both is in both lists with the same key; the speech
/// list leaves out its language models.
export function ServiceList({
  settings,
  onSettings,
  only,
  continued = false,
}: Props & { only?: "speech" | "text"; /** Rows come after another one in the same card. */ continued?: boolean }) {
  const { t } = useTranslation();
  const { named } = groupConnections(settings.connections);
  const actions = useConnections(onSettings);
  const [editing, setEditing] = useState<ServiceId | null>(null);
  const [key, setKey] = useState("");
  // The service whose key is being replaced, and the new key; its key removed meanwhile closes
  // the form for good.
  const [replacing, setReplacing] = useState<ServiceId | null>(null);
  const [newKey, setNewKey] = useState("");
  const replacedGone = replacing !== null && named[replacing] === undefined;
  useEffect(() => {
    if (replacedGone) {
      setReplacing(null);
      setNewKey("");
    }
  }, [replacedGone]);
  const ids = useId();
  // The form shows only while its service has no key (`keyFormOpen`); one added elsewhere
  // meanwhile closes it for good.
  const open = keyFormOpen(editing, settings.connections);
  useEffect(() => {
    if (editing !== null && open === null) {
      setEditing(null);
      setKey("");
    }
  }, [editing, open]);
  // A closed form or a removed key takes away the focused field or button: the focus goes to the
  // first control of the row.
  const follow = useFocusAfter();
  const rows = useRef<Partial<Record<ServiceId, HTMLDivElement | null>>>({});
  const toRow = (id: ServiceId) => follow(() => firstControl(rows.current[id]));

  async function submit(service: Service) {
    toRow(service.id);
    if (await actions.addKey(service, key)) {
      setEditing(null);
      setKey("");
    }
  }

  function closeReplace(service: ServiceId) {
    toRow(service);
    setReplacing(null);
    setNewKey("");
  }

  async function replace(service: ServiceId, connection: Connection) {
    toRow(service);
    if (await actions.replaceKey(connection, newKey)) closeReplace(service);
  }

  return (
    <>
      {servicesFor(only).map((service, index) => {
        const connection = named[service.id];
        const message = actions.message?.id === service.id || actions.message?.id === connection?.id ? actions.message : null;
        const messageId = `${ids}-${service.id}`;
        return (
          <ServiceRow
            key={service.id}
            rowRef={(element) => {
              rows.current[service.id] = element;
            }}
            first={index === 0 && !continued}
            monogram={service.monogram}
            logo={service.id}
            name={t(`connections.services.${service.id}`)}
            service={service}
            listsModels={only !== "speech"}
            connection={connection}
            busy={actions.busy}
            removing={actions.removing === connection?.id}
            message={message}
            messageId={messageId}
            onRefresh={() => {
              if (!connection) return;
              // Disabled while the models are read: the focus comes back to it.
              toRow(service.id);
              void actions.refresh(connection);
            }}
            onRemove={() => connection && actions.remove(connection, () => toRow(service.id))}
            onReplace={
              replacing === service.id
                ? undefined
                : () => {
                    setReplacing(service.id);
                    setNewKey("");
                    actions.setMessage(null);
                  }
            }
            action={
              open === service.id ? undefined : (
                <button
                  type="button"
                  className="btn compact"
                  disabled={actions.busy !== null}
                  onClick={() => {
                    setEditing(service.id);
                    setKey("");
                    actions.setMessage(null);
                  }}
                >
                  {t("connections.add_key")}
                </button>
              )
            }
          >
            {open === service.id && (
              <KeyForm
                label={t("connections.key_for", { service: t(`connections.services.${service.id}`) })}
                value={key}
                onChange={setKey}
                busy={actions.busy === service.id}
                describedBy={message?.error ? messageId : undefined}
                onSubmit={() => submit(service)}
                onCancel={() => {
                  toRow(service.id);
                  setEditing(null);
                  setKey("");
                }}
              />
            )}
            {connection && replacing === service.id && (
              <KeyForm
                label={t("connections.new_key_for", { service: t(`connections.services.${service.id}`) })}
                value={newKey}
                onChange={setNewKey}
                busy={actions.busy === connection.id}
                describedBy={message?.error ? messageId : undefined}
                submitLabel={t("connections.replace")}
                onSubmit={() => void replace(service.id, connection)}
                onCancel={() => closeReplace(service.id)}
              />
            )}
          </ServiceRow>
        );
      })}
    </>
  );
}

/// The key form of one named service for a row outside this panel: a cloud engine without its
/// key. Closes once the key is added. `onLeave`: called as the form may close (a key sent,
/// "Cancel", Escape), which takes away the focused field or button. `onBusy`: whether the key is
/// being checked; `false` once the form is gone.
export function ServiceKeyForm({
  service,
  onSettings,
  onLeave,
  onClose,
  onBusy,
}: {
  service: Service;
  onSettings: (settings: Settings) => void;
  onLeave: () => void;
  onClose: () => void;
  onBusy?: (busy: boolean) => void;
}) {
  const { t } = useTranslation();
  const actions = useConnections(onSettings);
  const [key, setKey] = useState("");
  const messageId = useId();
  const refused = actions.message?.error === true;
  const busy = actions.busy === service.id;
  const tell = useRef(onBusy);
  tell.current = onBusy;
  useEffect(() => tell.current?.(busy), [busy]);
  useEffect(() => () => tell.current?.(false), []);
  return (
    <>
      <KeyForm
        label={t("connections.key_for", { service: t(`connections.services.${service.id}`) })}
        value={key}
        onChange={setKey}
        busy={busy}
        indent={false}
        describedBy={refused ? messageId : undefined}
        onSubmit={async () => {
          onLeave();
          if (await actions.addKey(service, key)) onClose();
        }}
        onCancel={() => {
          onLeave();
          onClose();
        }}
      />
      {refused && (
        <p id={messageId} className="caption warning-text" role="alert">
          {actions.message?.text}
        </p>
      )}
    </>
  );
}

function ServiceRow({
  rowRef,
  first,
  monogram: letters,
  logo,
  name,
  service,
  listsModels = true,
  connection,
  busy,
  removing,
  message,
  messageId,
  onRefresh,
  onRemove,
  onReplace,
  action,
  children,
}: {
  rowRef?: Ref<HTMLDivElement>;
  first: boolean;
  monogram: string;
  /// The service's mark instead of the letters.
  logo?: LogoId;
  name: string;
  /// A named service: its capabilities are shown. Own servers have language models only.
  service?: Service;
  /// Shows the count of language models and their refresh; off in the speech list.
  listsModels?: boolean;
  connection: Connection | undefined;
  busy: string | null;
  removing: boolean;
  message: { text: string; error: boolean } | null;
  /// The id of the message, for the key field it speaks of.
  messageId?: string;
  onRefresh: () => void;
  onRemove: () => void;
  /// Opens the form of a new key in place of the connection's; none while it is open.
  onReplace?: () => void;
  /// Shown while the service is not connected.
  action?: ReactNode;
  children?: ReactNode;
}) {
  const { t, i18n } = useTranslation();
  const date = (ms: number) => new Date(ms).toLocaleDateString(i18n.language);
  const text = (service?.text ?? true) && listsModels;
  return (
    <div ref={rowRef} className={first ? "card-row service-row" : "card-row separator service-row"}>
      <div className="card-row-main">
        <ServiceMark logo={logo} letters={letters} />
        <div className="card-row-text">
          <span className="service-title">
            <span className="strong">{name}</span>
            {service?.speech && <span className="cap-chip">{t("connections.speech")}</span>}
            {service?.text && <span className="cap-chip">{t("connections.text")}</span>}
            {service && (
              <IconButton
                icon="help"
                size={22}
                label={t("connections.where_key", { service: name })}
                onClick={() => void openUrl(service.keysUrl).catch(() => {})}
              />
            )}
          </span>
          {connection ? (
            <span className="key-meta">
              {connection.key_hint ?? t("connections.no_key")} · {t("connections.added", { date: date(connection.added_at_ms) })}
              {text && ` · ${t("connections.model_count", { number: connection.models.length })}`}
            </span>
          ) : (
            <span className="caption">{t("connections.not_connected")}</span>
          )}
        </div>
        <div className="card-row-control">
          {connection ? (
            <>
              {text && (
                <button type="button" className="btn link" disabled={busy !== null} onClick={onRefresh}>
                  {busy === connection.id ? t("connections.refreshing") : t("connections.refresh")}
                </button>
              )}
              {onReplace && connection.key_hint !== null && (
                <button type="button" className="btn link" disabled={busy !== null} onClick={onReplace}>
                  {t("connections.replace_key")}
                </button>
              )}
              <button type="button" className="btn link quiet" disabled={busy !== null} onClick={onRemove}>
                {removing ? t("connections.confirm_remove") : t("connections.remove")}
              </button>
            </>
          ) : (
            action
          )}
        </div>
      </div>
      {children}
      {message && (
        <p id={messageId} className={message.error ? "caption warning-text service-status" : "caption service-status"} role={messageRole(message.error)}>
          {message.text}
        </p>
      )}
    </div>
  );
}

function KeyForm({
  label,
  value,
  onChange,
  busy,
  optional = false,
  indent,
  describedBy,
  onSubmit,
  onCancel,
  submitLabel,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  busy: boolean;
  /// What the button that sends the key says; "Connect" by default.
  submitLabel?: string;
  /// The id of the message that says why the key was refused.
  describedBy?: string;
  /// A server's key: under the server's address, not under a named service.
  optional?: boolean;
  /// Lined up with the service's name in its row.
  indent?: boolean;
  onSubmit: () => void;
  onCancel: () => void;
}) {
  const { t } = useTranslation();
  // A refused key leaves the form open: the focus, thrown out while it was checked, goes back to
  // the key.
  const field = useRef<HTMLInputElement>(null);
  useFocusAfterBusy(busy, field);
  return (
    <div className={(indent ?? !optional) ? "key-form service-form" : "key-form"}>
      <input
        ref={field}
        className="text-input"
        type="password"
        autoComplete="off"
        // A server's form starts at its address.
        autoFocus={!optional}
        aria-label={label}
        aria-describedby={describedBy}
        placeholder={t(optional ? "connections.key_placeholder_optional" : "connections.key_placeholder")}
        value={value}
        disabled={busy}
        onChange={(event) => onChange(event.currentTarget.value)}
        onKeyDown={(event) => {
          if (isComposing(event)) return;
          if (event.key === "Enter" && !busy) onSubmit();
          if (event.key === "Escape") onCancel();
        }}
      />
      <button type="button" className="btn primary" disabled={busy} onClick={onSubmit}>
        {busy ? t("connections.checking") : (submitLabel ?? t("connections.connect"))}
      </button>
      <button type="button" className="btn link" disabled={busy} onClick={onCancel}>
        {t("connections.cancel")}
      </button>
    </div>
  );
}

/// What the form of a new own server holds.
export interface ServerDraft {
  name: string;
  baseUrl: string;
  key: string;
}

/// The form of a new own server as it opens: empty, whatever a form closed before held, so a key
/// typed there never goes to another address.
export const NEW_SERVER: ServerDraft = { name: "", baseUrl: "", key: "" };

/// Servers with an OpenAI-style API at their own address: Ollama, LM Studio, OpenRouter… A
/// server on this computer needs no key.
function OwnServers({ settings, onSettings }: Props) {
  const { t } = useTranslation();
  const { own } = groupConnections(settings.connections);
  const actions = useConnections(onSettings);
  // The open form; `null` while it is closed, so what it held goes with it.
  const [draft, setDraft] = useState<ServerDraft | null>(null);
  const edit = (change: Partial<ServerDraft>) => setDraft((current) => current && { ...current, ...change });
  const messageId = useId();
  // A closed form or a removed server takes away the focused field or button: the focus goes to
  // "Add server", or from a removed server to the next one.
  const follow = useFocusAfter();
  const addButton = useRef<HTMLButtonElement>(null);
  const rows = useRef<Record<string, HTMLDivElement | null>>({});
  const toAdd = () => follow(() => addButton.current);

  function remove(connection: Connection) {
    const before = own.map((item) => item.id);
    actions.remove(connection, () =>
      follow(() => {
        const left = Object.keys(rows.current).filter((id) => rows.current[id]);
        const next = rowAfterRemoval(before, connection.id, left);
        return next !== null ? firstControl(rows.current[next]) : addButton.current;
      }),
    );
  }

  async function submit() {
    if (!draft) return;
    toAdd();
    const address = draft.baseUrl.trim();
    if (!address) return actions.setMessage({ id: "new", text: t("connections.error.invalid_base_url"), error: true });
    const added = await actions.add("new", {
      provider: "open_ai_compatible",
      name: draft.name.trim() || hostOf(address),
      baseUrl: address,
      apiKey: draft.key,
    });
    if (added) setDraft(null);
  }

  return (
    <Card title={t("connections.own_title")}>
      <p className="caption card-note">{t("connections.own_note")}</p>
      {own.map((connection) => (
        <ServiceRow
          key={connection.id}
          rowRef={(element) => {
            rows.current[connection.id] = element;
          }}
          first={false}
          monogram={monogram(connection)}
          name={connection.name}
          connection={connection}
          busy={actions.busy}
          removing={actions.removing === connection.id}
          message={actions.message?.id === connection.id ? actions.message : null}
          onRefresh={() => {
            // Disabled while the models are read: the focus comes back to it.
            follow(() => firstControl(rows.current[connection.id]));
            void actions.refresh(connection);
          }}
          onRemove={() => remove(connection)}
        />
      ))}
      {draft ? (
        <>
          <Row
            title={t("connections.base_url")}
            detail={t("connections.base_url_hint")}
            control={
              <input
                className="text-input"
                aria-label={t("connections.base_url")}
                value={draft.baseUrl}
                autoFocus
                placeholder="http://localhost:11434/v1"
                onChange={(event) => edit({ baseUrl: event.currentTarget.value })}
              />
            }
          />
          <Row
            title={t("connections.name")}
            detail={t("connections.name_hint")}
            control={
              <input
                className="text-input"
                aria-label={t("connections.name")}
                value={draft.name}
                placeholder={draft.baseUrl.trim() ? hostOf(draft.baseUrl) : "Ollama"}
                onChange={(event) => edit({ name: event.currentTarget.value })}
              />
            }
          />
          <Row title={t("connections.key")} detail={t("connections.key_optional")}>
            <KeyForm
              label={t("connections.key")}
              value={draft.key}
              onChange={(key) => edit({ key })}
              busy={actions.busy === "new"}
              optional
              describedBy={actions.message?.id === "new" && actions.message.error ? messageId : undefined}
              onSubmit={submit}
              onCancel={() => {
                toAdd();
                setDraft(null);
                actions.setMessage(null);
              }}
            />
            {actions.message?.id === "new" && (
              <p id={messageId} className={actions.message.error ? "caption warning-text" : "caption"} role={messageRole(actions.message.error)}>
                {actions.message.text}
              </p>
            )}
          </Row>
        </>
      ) : (
        <div className="card-row separator">
          <button
            ref={addButton}
            type="button"
            className="btn compact"
            disabled={actions.busy !== null}
            onClick={() => {
              setDraft(NEW_SERVER);
              actions.setMessage(null);
            }}
          >
            {t("connections.own_add")}
          </button>
          {actions.message?.id === "new" && !actions.message.error && (
            <span className="caption own-added" role="status">
              {actions.message.text}
            </span>
          )}
        </div>
      )}
    </Card>
  );
}
