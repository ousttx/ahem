// Building blocks of the window; their styles are in App.css.
import { useCallback, useEffect, useId, useLayoutEffect, useRef, useState, type CSSProperties, type KeyboardEvent, type ReactNode, type RefObject } from "react";
import { createPortal } from "react-dom";
import { formatHotkey, hotkeyKeys } from "./dictation";
import { meter } from "./present";

/// Stroke icons, 24×24.
const PATHS: Record<string, ReactNode> = {
  microphone: (
    <>
      <rect x="9" y="3" width="6" height="12" rx="3" />
      <path d="M5 11v1a7 7 0 0 0 14 0v-1M12 19v3M8 22h8" />
    </>
  ),
  file: <path d="M14 2H5v20h14V7zM14 2v5h5M8 12h8M8 16h6" />,
  book: <path d="M5 4.5A1.5 1.5 0 0 1 6.5 3H19v15H6.5A1.5 1.5 0 0 0 5 19.5zM5 19.5A1.5 1.5 0 0 0 6.5 21H19v-3M9 7h6M9 10.5h4" />,
  down: <path d="m6 9 6 6 6-6" />,
  wave: <path d="M4 10v4M8 6v12M12 3v18M16 7v10M20 10v4" />,
  warning: <path d="m12 3 10 18H2zM12 9v5M12 17v1" />,
  check: <path d="m5 12 4 4L19 6" />,
  sparkle: (
    <path d="M11 3l1.7 4.6 4.6 1.7-4.6 1.7L11 15.6l-1.7-4.6-4.6-1.7 4.6-1.7zM18 14l.9 2.1L21 17l-2.1.9L18 20l-.9-2.1L15 17l2.1-.9z" />
  ),
  chip: <path d="M7 7h10v10H7zM10 10h4v4h-4zM9 3v4M15 3v4M9 17v4M15 17v4M3 9h4M3 15h4M17 9h4M17 15h4" />,
  cursor: <path d="M9 4h6M9 20h6M12 4v16" />,
  copy: <path d="M9 9h11v11H9zM5 15H4V4h11v1" />,
  trash: <path d="M4 7h16M10 11v6M14 11v6M6 7l1 13h10l1-13M9 7V4h6v3" />,
  upload: <path d="M12 16V5M7 10l5-5 5 5M5 19h14" />,
  close: <path d="M6 6l12 12M18 6 6 18" />,
  minimize: <path d="M6 12h12" />,
  maximize: <rect x="6" y="6" width="12" height="12" rx="2" />,
  restore: (
    <>
      <rect x="5" y="9" width="10" height="10" rx="2" />
      <path d="M9 9V7a2 2 0 0 1 2-2h6a2 2 0 0 1 2 2v6a2 2 0 0 1-2 2h-2" />
    </>
  ),
  circle: <circle cx="12" cy="12" r="8" />,
  people: (
    <>
      <circle cx="9" cy="8" r="3.5" />
      <path d="M2.5 20a6.5 6.5 0 0 1 13 0M16 4.6a3.5 3.5 0 0 1 0 6.8M18.5 14.2A6.5 6.5 0 0 1 21.5 20" />
    </>
  ),
  help: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M9.6 9.4a2.5 2.5 0 1 1 3.6 2.3c-.7.4-1.2.9-1.2 1.7v.4M12 17.2v.3" />
    </>
  ),
  key: (
    <>
      <circle cx="7" cy="17" r="5" />
      <path d="M10.6 13.4 21 3M16 8l3.2 3.2M18.6 5.4l2.4 2.4" />
    </>
  ),
  clock: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 7v5l3 2" />
    </>
  ),
  // Feather Icons, "settings" (MIT, © Cole Bemis)
  gear: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </>
  ),
};

export type Tone = "text" | "accent" | "muted" | "faint" | "danger" | "ok";

export function Icon({ name, size = 16, tone = "text" }: { name: string; size?: number; tone?: Tone }) {
  return (
    <svg
      className={`icon tone-${tone}`}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.6}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      {PATHS[name]}
    </svg>
  );
}

/// Round state badge: a tinted circle with an icon and a halo while recording.
export function IconCircle({
  icon,
  tone,
  ring = false,
  diameter = 52,
  halo = 6,
}: {
  icon: string;
  tone: Tone;
  ring?: boolean;
  diameter?: number;
  halo?: number;
}) {
  return (
    <span
      className={`icon-circle tone-${tone}${ring ? " ring" : ""}`}
      style={{ width: diameter + 2 * halo, height: diameter + 2 * halo, padding: halo }}
    >
      <span className="disc">
        <Icon name={icon} size={Math.round(diameter * 0.46)} tone={tone} />
      </span>
    </span>
  );
}

export function Card({
  title,
  action,
  className,
  id,
  role,
  labelledBy,
  children,
}: {
  title?: string;
  action?: ReactNode;
  className?: string;
  /// For a card that is the panel of tabs: its id, `tabpanel` and the tab that names it.
  id?: string;
  role?: string;
  labelledBy?: string;
  children?: ReactNode;
}) {
  return (
    <section className={className ? `card ${className}` : "card"} id={id} role={role} aria-labelledby={labelledBy}>
      {title && (
        <header className="card-header">
          <h3 className="card-title">{title}</h3>
          {action}
        </header>
      )}
      {children}
    </section>
  );
}

/// A setting inside a card: name and caption on the left, the control on the right.
export function Row({
  title,
  detail,
  control,
  separator = true,
  strong = false,
  children,
}: {
  title: ReactNode;
  detail?: ReactNode;
  control?: ReactNode;
  separator?: boolean;
  strong?: boolean;
  /// Full-width content under the name, such as a text area.
  children?: ReactNode;
}) {
  return (
    <div className={separator ? "card-row separator" : "card-row"}>
      <div className="card-row-main">
        <div className="card-row-text">
          <div className={strong ? "strong" : undefined}>{title}</div>
          {detail && <div className="caption">{detail}</div>}
        </div>
        {control && <div className="card-row-control">{control}</div>}
      </div>
      {children}
    </div>
  );
}

export function Toggle({
  checked,
  onChange,
  label,
  disabled = false,
}: {
  checked: boolean;
  onChange?: (checked: boolean) => void;
  label: string;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className={checked ? "toggle on" : "toggle"}
      disabled={disabled}
      onClick={() => onChange?.(!checked)}
    >
      <span className="knob" />
    </button>
  );
}

/// Where an arrow key, Home or End takes the focus in a group of items (radios, tabs), from the
/// item `from`; `null` for any other key. Arrows go round the ends, and every step skips the
/// items that cannot be chosen.
export function rovingStep(key: string, from: number, enabled: readonly boolean[]): number | null {
  const count = enabled.length;
  const ends = key === "Home" || key === "End";
  const step = key === "ArrowRight" || key === "ArrowDown" || key === "Home" ? 1 : key === "ArrowLeft" || key === "ArrowUp" || key === "End" ? -1 : 0;
  if (step === 0) return null;
  const start = key === "Home" ? -1 : key === "End" ? count : from;
  for (let shift = 1; shift <= count; shift++) {
    const index = ends ? start + step * shift : (((start + step * shift) % count) + count) % count;
    if (index >= 0 && index < count && enabled[index]) return index;
  }
  return from;
}

/// The one item of a group that Tab stops at: the chosen one, else the first that can be chosen;
/// -1 when none can.
export function tabStop(items: readonly { checked: boolean; disabled?: boolean }[]): number {
  const chosen = items.findIndex((item) => item.checked && !item.disabled);
  return chosen >= 0 ? chosen : items.findIndex((item) => !item.disabled);
}

const ROVING_ITEMS = '[role="radio"], [role="tab"]';

/// A group of radios or tabs as the roles promise a screen reader: Tab stops once in it, at the
/// chosen item (`tabIndex`), and the arrows, Home and End move the focus to another item and
/// choose it. `group` goes on the element around the items; `items` are their states in order.
/// `manual`: the keys only move the focus, and Enter or Space chooses (the items are buttons), for
/// a group where choosing loses something, such as a draft.
export function useRoving(items: readonly { checked: boolean; disabled?: boolean }[], { manual = false }: { manual?: boolean } = {}) {
  const stop = tabStop(items);
  return {
    group: {
      onKeyDown: (event: KeyboardEvent<HTMLElement>) => {
        const elements = Array.from(event.currentTarget.querySelectorAll<HTMLElement>(ROVING_ITEMS));
        const from = elements.indexOf(event.target as HTMLElement);
        if (from < 0) return;
        const enabled = elements.map((element) => !(element as HTMLButtonElement).disabled);
        const next = rovingStep(event.key, from, enabled);
        if (next === null) return;
        event.preventDefault();
        if (next === from) return;
        elements[next].focus();
        if (!manual) elements[next].click();
      },
    },
    tabIndex: (index: number) => (index === stop ? 0 : -1),
  };
}

export function Segments<T extends string>({
  options,
  value,
  onChange,
  label,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (value: T) => void;
  label: string;
}) {
  const roving = useRoving(options.map((option) => ({ checked: option.value === value })));
  return (
    <div className="segments" role="radiogroup" aria-label={label} {...roving.group}>
      {options.map((option, index) => (
        <button
          key={option.value}
          type="button"
          role="radio"
          aria-checked={option.value === value}
          tabIndex={roving.tabIndex(index)}
          className={option.value === value ? "segment checked" : "segment"}
          // The bold text of the chosen segment must not change the width.
          data-text={option.label}
          onClick={() => onChange(option.value)}
        >
          {option.label}
        </button>
      ))}
    </div>
  );
}

/// The look of a radio button; the row around it takes the clicks.
export function Radio({ checked, available = true }: { checked: boolean; available?: boolean }) {
  return <span className={`radio${checked ? " checked" : ""}${available ? "" : " unavailable"}`} aria-hidden="true" />;
}

/// `pressed` keeps the keys down; a new `tap` presses them once, briefly.
export function KeyCaps({
  hotkey,
  large = false,
  pressed = false,
  tap = 0,
}: {
  hotkey: string;
  large?: boolean;
  pressed?: boolean;
  tap?: number;
}) {
  const className = ["keycaps", large && "large", pressed && "pressed", tap > 0 && !pressed && "tap"].filter(Boolean).join(" ");
  return (
    // A new key replays the tap.
    <span key={tap} className={className} aria-label={formatHotkey(hotkey)}>
      {hotkeyKeys(hotkey).map((part, index) => (
        <kbd key={index}>{part}</kbd>
      ))}
    </span>
  );
}

/// A selectable row of a list, marked with an accent bar when chosen: a title, a caption,
/// optional short tags under it and an optional trailing element. The row is a tab of the panel
/// `controls`.
export function ChoiceRow({
  id,
  controls,
  title,
  caption,
  tags = [],
  checked,
  onSelect,
  trailing,
  tabIndex,
}: {
  id: string;
  controls: string;
  title: string;
  caption?: string;
  tags?: string[];
  checked: boolean;
  onSelect: () => void;
  trailing?: ReactNode;
  /// Its place in the Tab order (`useRoving`).
  tabIndex?: number;
}) {
  return (
    <button
      type="button"
      role="tab"
      id={id}
      aria-controls={controls}
      aria-selected={checked}
      tabIndex={tabIndex}
      className={checked ? "choice-row checked" : "choice-row"}
      onClick={onSelect}
    >
      <span className="choice-texts">
        <span className="choice-title">{title}</span>
        {caption && (
          <span className="choice-caption" {...clipTitle(caption)}>
            {caption}
          </span>
        )}
        {tags.length > 0 && (
          <span className="choice-tags">
            {tags.map((tag) => (
              <span key={tag} className="choice-tag">
                {tag}
              </span>
            ))}
          </span>
        )}
      </span>
      {trailing}
    </button>
  );
}

/// One step of the dictation pipeline: icon and step name, current value, short note.
export function StepTile({
  icon,
  step,
  value,
  note,
  onClick,
}: {
  icon: string;
  step: string;
  value: string;
  note: string;
  onClick?: () => void;
}) {
  const content = (
    <>
      <span className="step-header">
        <Icon name={icon} size={15} tone="accent" />
        <span className="caption">{step}</span>
      </span>
      <span className="strong step-line" {...clipTitle(value)}>
        {value}
      </span>
      <span className="caption step-line" {...clipTitle(note)}>
        {note}
      </span>
    </>
  );
  return onClick ? (
    <button type="button" className="step-tile" onClick={onClick} aria-label={`${step}: ${value}. ${note}`}>
      {content}
    </button>
  ) : (
    <div className="step-tile">{content}</div>
  );
}

export interface SelectOption {
  value: string;
  label: string;
  disabled?: boolean;
}

/// The next option that can be chosen, one `step` away; at an end it stays at `from`. `from` may
/// be -1 or the length to start from either end.
export function stepOption(options: SelectOption[], from: number, step: 1 | -1): number {
  for (let index = from + step; index >= 0 && index < options.length; index += step) {
    if (!options[index].disabled) return index;
  }
  return from;
}

/// The first option that can be chosen whose label starts with `typed` (any case), searching
/// after `from` and around; -1 when none does.
export function typedOption(options: SelectOption[], typed: string, from: number): number {
  const prefix = typed.toLocaleLowerCase();
  for (let shift = 1; shift <= options.length; shift++) {
    const index = (((from + shift) % options.length) + options.length) % options.length;
    const option = options[index];
    if (!option.disabled && option.label.toLocaleLowerCase().startsWith(prefix)) return index;
  }
  return -1;
}

const LIST_ROW = 31;
const LIST_MAX_HEIGHT = 320;
const LIST_MARGIN = 8;
const TYPE_AHEAD_MS = 700;

interface ListPlace {
  left: number;
  top?: number;
  bottom?: number;
  minWidth: number;
  maxWidth: number;
  maxHeight: number;
}

/// A drop-down in the app's style (the system one does not follow it). The open list floats over
/// the page below the field, or above it when there is more room there; arrows, Home, End,
/// Enter, Space, Escape, Tab and typing the first letters work as in a system list.
export function Select({
  value,
  options,
  onChange,
  label,
  disabled = false,
  width,
}: {
  value: string;
  options: SelectOption[];
  onChange: (value: string) => void;
  label: string;
  disabled?: boolean;
  width?: number;
}) {
  const [place, setPlace] = useState<ListPlace | null>(null);
  const [active, setActive] = useState(-1);
  const trigger = useRef<HTMLButtonElement>(null);
  const list = useRef<HTMLDivElement>(null);
  const typed = useRef({ text: "", at: 0 });
  const id = useId();
  const open = place !== null;
  const chosen = options.findIndex((option) => option.value === value);

  function show() {
    const box = trigger.current?.getBoundingClientRect();
    if (!box) return;
    const below = window.innerHeight - box.bottom - LIST_MARGIN;
    const above = box.top - LIST_MARGIN;
    const wanted = Math.min(LIST_MAX_HEIGHT, options.length * LIST_ROW + 10);
    const down = below >= wanted || below >= above;
    setPlace({
      left: box.left,
      ...(down ? { top: box.bottom + 4 } : { bottom: window.innerHeight - box.top + 4 }),
      minWidth: box.width,
      maxWidth: Math.max(box.width, window.innerWidth - box.left - LIST_MARGIN),
      maxHeight: Math.max(LIST_ROW * 3, Math.min(LIST_MAX_HEIGHT, down ? below : above)),
    });
    setActive(chosen >= 0 && !options[chosen].disabled ? chosen : stepOption(options, -1, 1));
  }

  function choose(index: number) {
    const option = options[index];
    if (!option || option.disabled) return;
    setPlace(null);
    if (option.value !== value) onChange(option.value);
  }

  // The list closes on a click elsewhere, when the page scrolls or the window changes size (it
  // stays where it was opened) and when the window loses the focus.
  useEffect(() => {
    if (!open) return;
    const close = () => setPlace(null);
    const outside = (event: PointerEvent) => {
      const target = event.target as Node;
      if (!trigger.current?.contains(target) && !list.current?.contains(target)) close();
    };
    const scrolled = (event: Event) => {
      if (!list.current?.contains(event.target as Node)) close();
    };
    document.addEventListener("pointerdown", outside, true);
    window.addEventListener("scroll", scrolled, true);
    window.addEventListener("resize", close);
    window.addEventListener("blur", close);
    return () => {
      document.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("scroll", scrolled, true);
      window.removeEventListener("resize", close);
      window.removeEventListener("blur", close);
    };
  }, [open]);

  useEffect(() => {
    if (open && active >= 0) list.current?.querySelector(`[data-index="${active}"]`)?.scrollIntoView({ block: "nearest" });
  }, [open, active]);

  function onKeyDown(event: KeyboardEvent) {
    const key = event.key;
    if (!open) {
      if (key === "ArrowDown" || key === "ArrowUp" || key === "Enter" || key === " ") {
        event.preventDefault();
        show();
      }
      return;
    }
    if (key === "ArrowDown" || key === "ArrowUp") {
      event.preventDefault();
      setActive((index) => stepOption(options, index, key === "ArrowDown" ? 1 : -1));
    } else if (key === "Home" || key === "End") {
      event.preventDefault();
      setActive(key === "Home" ? stepOption(options, -1, 1) : stepOption(options, options.length, -1));
    } else if (key === "Enter" || key === " ") {
      event.preventDefault();
      choose(active);
    } else if (key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      setPlace(null);
    } else if (key === "Tab") {
      setPlace(null);
    } else if (key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      const now = Date.now();
      const text = now - typed.current.at < TYPE_AHEAD_MS ? typed.current.text + key : key;
      typed.current = { text, at: now };
      // A longer prefix may still match the highlighted option; a first letter moves on.
      const found = typedOption(options, text, text.length > 1 ? active - 1 : active);
      if (found >= 0) setActive(found);
    }
  }

  return (
    <span className="select" style={width ? { width } : undefined}>
      <button
        ref={trigger}
        type="button"
        className="select-trigger"
        role="combobox"
        aria-label={label}
        aria-haspopup="listbox"
        aria-expanded={open}
        aria-controls={open ? id : undefined}
        aria-activedescendant={open && active >= 0 ? `${id}-${active}` : undefined}
        disabled={disabled}
        onClick={() => (open ? setPlace(null) : show())}
        onKeyDown={onKeyDown}
      >
        {chosen >= 0 ? options[chosen].label : ""}
      </button>
      <Icon name="down" size={13} tone="muted" />
      {place &&
        createPortal(
          // The focus stays on the field: keys go there, a click on an option does not take it.
          <div
            ref={list}
            id={id}
            className="select-list"
            role="listbox"
            aria-label={label}
            style={place}
            onPointerDown={(event) => event.preventDefault()}
          >
            {options.map((option, index) => (
              <div
                key={option.value}
                id={`${id}-${index}`}
                data-index={index}
                role="option"
                aria-selected={index === chosen}
                aria-disabled={option.disabled || undefined}
                className={index === active ? "select-option active" : "select-option"}
                onPointerMove={() => !option.disabled && setActive(index)}
                onClick={() => choose(index)}
              >
                <span className="select-option-label">{option.label}</span>
                {index === chosen && <Icon name="check" size={14} />}
              </div>
            ))}
          </div>,
          document.body,
        )}
    </span>
  );
}

/// The id of the panel of the tabs `id`.
export const tabPanelId = (tabs: string) => `${tabs}-panel`;

/// Tabs over one panel: each tab's id is `${id}-${value}`, the panel's is `tabPanelId(id)`.
export function Tabs<T extends string>({
  id,
  tabs,
  value,
  onChange,
}: {
  id: string;
  tabs: { value: T; label: string; disabled?: boolean }[];
  value: T;
  onChange: (value: T) => void;
}) {
  const roving = useRoving(tabs.map((tab) => ({ checked: tab.value === value, disabled: tab.disabled })));
  return (
    <div className="tabs" role="tablist" {...roving.group}>
      {tabs.map((tab, index) => (
        <button
          key={tab.value}
          type="button"
          role="tab"
          id={`${id}-${tab.value}`}
          aria-controls={tabPanelId(id)}
          aria-selected={tab.value === value}
          tabIndex={roving.tabIndex(index)}
          className={tab.value === value ? "tab selected" : "tab"}
          data-text={tab.label}
          disabled={tab.disabled}
          onClick={() => onChange(tab.value)}
        >
          {tab.label}
        </button>
      ))}
    </div>
  );
}

export function IconButton({
  icon,
  label,
  onClick,
  tone = "muted",
  size = 30,
  disabled = false,
}: {
  icon: string;
  label: string;
  onClick: () => void;
  tone?: Tone;
  size?: number;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      className="icon-button"
      style={{ width: size, height: size }}
      title={label}
      aria-label={label}
      disabled={disabled}
      onClick={onClick}
    >
      <Icon name={icon} size={15} tone={disabled ? "faint" : tone} />
    </button>
  );
}

/// The single place for messages about the whole window, such as a rejected change.
export function InfoBar({ text, error, onClose, closeLabel }: { text: string; error: boolean; onClose: () => void; closeLabel: string }) {
  return (
    <div className="info-bar-margin">
      <div className={error ? "info-bar error" : "info-bar"} role={error ? "alert" : "status"}>
        <Icon name={error ? "warning" : "check"} size={16} tone={error ? "danger" : "accent"} />
        <span className="info-bar-text">{text}</span>
        <IconButton icon="close" label={closeLabel} size={24} onClick={onClose} />
      </div>
    </div>
  );
}

/// What `isClipped` reads of an element.
interface Box {
  scrollWidth: number;
  clientWidth: number;
  children?: ArrayLike<Box>;
}

/// Whether the element or anything in it shows less text than it holds (an ellipsis).
export function isClipped(element: Box): boolean {
  if (element.scrollWidth > element.clientWidth) return true;
  return Array.from(element.children ?? []).some(isClipped);
}

/// A hover tooltip with the whole text, only while the text does not fit: a tooltip that
/// repeats what is already readable is noise.
export function clipTitle(text: string) {
  return {
    onMouseEnter: (event: { currentTarget: HTMLElement }) => {
      const element = event.currentTarget;
      if (isClipped(element)) element.title = text;
      else element.removeAttribute("title");
    },
  };
}

/// Recognition engine state at the bottom of the navigation pane.
export function EngineStatus({ title, note, tone }: { title: string; note: string; tone: Tone }) {
  return (
    <div className="engine-status" {...clipTitle(`${title}\n${note}`)}>
      <span className={`status-dot tone-${tone}`} />
      <span className="engine-status-texts">
        <span className="small-strong">{title}</span>
        <span className="caption">{note}</span>
      </span>
    </div>
  );
}

/// The title of what `root` shows: its first `h1` that no hidden element holds. Pages, and the
/// screens of a page such as the file view of Transcripts, stay mounted while hidden.
export function shownTitle(root: Pick<ParentNode, "querySelectorAll">): HTMLElement | null {
  return Array.from(root.querySelectorAll<HTMLElement>("h1")).find((title) => title.closest("[hidden]") === null) ?? null;
}

/// Calls `read` now and whenever the window gets the focus back, until the returned function is
/// called: what the OS has, such as its microphones, may have changed meanwhile.
export function readOnFocus(read: () => void): () => void {
  read();
  window.addEventListener("focus", read);
  return () => window.removeEventListener("focus", read);
}

/// Calls `seen` now and whenever the web view says the window is shown or hidden with whether it
/// is seen, until the returned function is called. WebKit says so for a window hidden in the
/// tray or minimized; WebView2 only for a web view its host hides.
export function watchSeen(seen: (shown: boolean) => void): () => void {
  const tell = () => seen(document.visibilityState === "visible");
  tell();
  document.addEventListener("visibilitychange", tell);
  return () => document.removeEventListener("visibilitychange", tell);
}

/// Whether the web view says the window is seen (`watchSeen`): a page of a window hidden in the
/// tray has nothing to draw.
export function useWindowSeen(): boolean {
  const [seen, setSeen] = useState(() => document.visibilityState === "visible");
  useEffect(() => watchSeen(setSeen), []);
  return seen;
}

/// Height of a bar of a level strip in CSS pixels, on the decibel scale of the indicator's
/// capsule (`meter`): silence stays at the bottom, speech moves the bars.
export function waveBarHeight(level: number): number {
  return 6 + meter(level) * 22;
}

/// Input levels as bars, newest on the right; while recording all of them are in the signal
/// color, like the capsule's.
export function WaveStrip({ levels, recording }: { levels: number[]; recording: boolean }) {
  return (
    <span className="wave-strip" aria-hidden="true">
      {levels.map((level, index) => (
        <span key={index} className={recording ? "bar live" : "bar"} style={{ height: waveBarHeight(level) }} />
      ))}
    </span>
  );
}

/// Where the highlight of a list goes: over the item `selector` finds in `container`, which must
/// be its offset parent (`position: relative`). It slides when `selected` changes and jumps
/// when the layout changes or when `remeasure` does, such as at each opening of a panel.
/// `box` places an element behind transparent items; `clip` cuts a copy of the list laid over
/// it down to the item, for items with their own background.
export function useThumb(
  container: RefObject<HTMLElement | null>,
  selector: string,
  selected: unknown,
  remeasure?: unknown,
  shape: "box" | "clip" = "box",
): CSSProperties {
  const hidden: CSSProperties = shape === "box" ? { opacity: 0 } : { clipPath: "inset(50%)" };
  const [style, setStyle] = useState<CSSProperties>(hidden);
  const last = useRef<unknown>(undefined);
  useLayoutEffect(() => {
    const box = container.current;
    if (!box) return;
    const place = (slide: boolean) => {
      const item = box.querySelector<HTMLElement>(selector);
      const still = slide ? {} : { transition: "none" };
      if (!item) return setStyle(hidden);
      if (shape === "clip") {
        const right = box.clientWidth - item.offsetLeft - item.offsetWidth;
        const bottom = box.clientHeight - item.offsetTop - item.offsetHeight;
        return setStyle({ clipPath: `inset(${item.offsetTop}px ${right}px ${bottom}px ${item.offsetLeft}px round 20px)`, ...still });
      }
      setStyle({
        width: item.offsetWidth,
        height: item.offsetHeight,
        transform: `translate(${item.offsetLeft}px, ${item.offsetTop}px)`,
        ...still,
      });
    };
    const first = last.current === undefined;
    place(!first && last.current !== selected);
    last.current = selected;
    // The observer reports once right away: only a real change of size may stop a slide.
    let size = `${box.clientWidth}×${box.clientHeight}`;
    const observer = new ResizeObserver(() => {
      const now = `${box.clientWidth}×${box.clientHeight}`;
      if (now === size) return;
      size = now;
      place(false);
    });
    observer.observe(box);
    return () => observer.disconnect();
  }, [container, selector, selected, remeasure, shape]);
  return style;
}

/// How often a level strip moves on.
const STRIP_MS = 80;

/// Input levels for a strip of bars, newest last. While `active` the strip moves on every 80 ms
/// with the latest level, also when the level stays the same, so it keeps running. `level` is the
/// level, or a ref the strip reads at each step (`useLevel`), so its changes draw nothing else.
export function useLevelStrip(level: number | RefObject<number>, active: boolean, bars: number): number[] {
  const [levels, setLevels] = useState<number[]>(() => Array(bars).fill(0));
  const latest = useRef(level);
  latest.current = level;
  useEffect(() => {
    if (!active) {
      setLevels(Array(bars).fill(0));
      return;
    }
    const read = () => (typeof latest.current === "number" ? latest.current : latest.current.current);
    const timer = setInterval(() => setLevels((list) => [...list.slice(1), read()]), STRIP_MS);
    return () => clearInterval(timer);
  }, [active, bars]);
  return levels;
}

/// Milliseconds since `running` became true, or since `since` (a `Date.now()`) when given,
/// counted every `every` ms while it stays so. A new run starts before the first paint, never at
/// the time of the run before.
export function useElapsed(running: boolean, every: number, since?: number): number {
  const [elapsed, setElapsed] = useState(0);
  useLayoutEffect(() => {
    if (!running) return;
    const start = since ?? Date.now();
    setElapsed(Math.max(0, Date.now() - start));
    const timer = setInterval(() => setElapsed(Math.max(0, Date.now() - start)), every);
    return () => clearInterval(timer);
  }, [running, every, since]);
  return elapsed;
}

/// A save that waits `delay` ms after the last `schedule`, so typing saves once it pauses; `flush`
/// saves at once (a field that loses the focus) and drops the one waiting. The save that waits is
/// the one given at its `schedule`. `timer` holds the save waiting.
export function debouncedSave<T>(
  timer: { current: ReturnType<typeof setTimeout> | null },
  save: (value: T) => void,
  delay: number,
): { schedule: (value: T) => void; flush: (value: T) => void } {
  const flush = (value: T) => {
    if (timer.current) clearTimeout(timer.current);
    timer.current = null;
    save(value);
  };
  return {
    schedule(value) {
      if (timer.current) clearTimeout(timer.current);
      timer.current = setTimeout(() => flush(value), delay);
    },
    flush,
  };
}

/// `debouncedSave` for a field: a save still waiting when the component goes is dropped.
export function useDebouncedSave<T>(save: (value: T) => void, delay: number): { schedule: (value: T) => void; flush: (value: T) => void } {
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );
  return debouncedSave(timer, save, delay);
}

/// The ids of `ids` that are not in `known`.
export function arrived<T>(known: ReadonlySet<T>, ids: readonly T[]): T[] {
  return ids.filter((id) => !known.has(id));
}

/// The row the focus goes to once the row `removed` of `before` is gone: the next one still in
/// `after`, which is in its place now, else the nearest above it; `null` when none is left (the
/// focus goes to the page's title).
export function rowAfterRemoval<T>(before: readonly T[], removed: T, after: readonly T[]): T | null {
  const left = new Set(after);
  const at = before.indexOf(removed);
  if (at < 0) return null;
  const below = before.slice(at + 1).find((id) => left.has(id));
  if (below !== undefined) return below;
  return before.slice(0, at).reverse().find((id) => left.has(id)) ?? null;
}

/// Whether the focus (`active`) is inside `container`.
export function focusWithin(container: Pick<Node, "contains"> | null | undefined, active: Element | null): boolean {
  return active !== null && container?.contains(active) === true;
}

/// Whether the focus is nowhere, as when the button that had it went with its row.
export function focusLost(): boolean {
  const active = document.activeElement;
  return active === null || active === document.body;
}

/// Whether the focus goes back to a field as a wait ends (`wasBusy`, now not `busy`): only when
/// it is nowhere (`lost`), thrown out by the field and its buttons disabled while busy, not when
/// the user put it elsewhere meanwhile.
export function focusReturns(wasBusy: boolean, busy: boolean, lost: boolean): boolean {
  return wasBusy && !busy && lost;
}

/// Puts the focus back in `field` once `busy` ends, while the field is still there: a check that
/// was refused leaves its form open to try again (`focusReturns`).
export function useFocusAfterBusy(busy: boolean, field: RefObject<HTMLElement | null>) {
  const wasBusy = useRef(busy);
  useEffect(() => {
    if (focusReturns(wasBusy.current, busy, focusLost())) field.current?.focus();
    wasBusy.current = busy;
  }, [busy, field]);
}

/// An action followed by `useFocusAfter`: the element that had the focus as it began (`from`),
/// where the focus goes once that element is gone (`find`, read from what is drawn then), and
/// whether `from` threw the focus out by being disabled while the action ran (`thrown`).
/// `disabledIsGone`: the action may leave `from` disabled for good (a bound reached, a file
/// deleted), so `from` disabled counts as gone; a target found disabled is looked for again at
/// every drawing, so another one that can be pressed first takes the focus.
export interface FocusWait<E> {
  from: E;
  find: () => E | null | undefined;
  thrown: boolean;
  disabledIsGone?: boolean;
}

const isDisabled = (element: object) => "disabled" in element && element.disabled === true;

/// One drawing while an action is followed (`useFocusAfter`): the element to focus now, if any,
/// and what is still waited for. The wait goes on while `from` has the focus, or lost it by being
/// disabled while the action runs. Once `from` is gone and the focus with it, the focus goes
/// where `find` says; a target still disabled for the action takes it once enabled. A `from`
/// enabled again after it threw the focus out (the action failed and left it) takes it back;
/// given back by someone else (`useFocusAfterBusy`), it has it again and threw nothing out. The
/// wait ends when the user has put the focus elsewhere (`lost` is false).
export function followFocus<E extends { isConnected: boolean }>(
  wait: FocusWait<E>,
  active: unknown,
  lost: boolean,
): { focus: E | null; wait: FocusWait<E> | null } {
  const gone = !wait.from.isConnected || (wait.disabledIsGone === true && isDisabled(wait.from));
  if (active === wait.from && !gone) return { focus: null, wait: wait.thrown ? { ...wait, thrown: false } : wait };
  // A disabled `from` may keep the focus in some web views: it is not elsewhere.
  if (!lost && active !== wait.from) return { focus: null, wait: null };
  if (gone) {
    const target = wait.find();
    if (!target) return { focus: null, wait: null };
    if (isDisabled(target)) return { focus: null, wait: { from: target, find: wait.find, thrown: true, disabledIsGone: wait.disabledIsGone } };
    return { focus: target, wait: null };
  }
  if (isDisabled(wait.from)) return { focus: null, wait: { ...wait, thrown: true } };
  return wait.thrown ? { focus: wait.from, wait: { ...wait, thrown: false } } : { focus: null, wait: null };
}

/// Keeps the focus on the page through an action that takes away the element that has it: a
/// deleted row, a closed form, a button replaced by another. `follow(find)` is called as the
/// action begins, with the focus still on its element; once a drawing has taken that element
/// away, the focus goes to what `find` gives then (`followFocus`). With `disabledIsGone`, an
/// element the action leaves disabled is taken away too. Without a focus to keep (a click on
/// macOS does not focus a button), nothing is followed.
export function useFocusAfter(): (find: () => HTMLElement | null | undefined, options?: { disabledIsGone?: boolean }) => void {
  const wait = useRef<FocusWait<HTMLElement> | null>(null);
  // After every drawing of the component: the change may come with any of them.
  useEffect(() => {
    if (!wait.current) return;
    const step = followFocus(wait.current, document.activeElement, focusLost());
    wait.current = step.wait;
    step.focus?.focus();
  });
  return useCallback((find: () => HTMLElement | null | undefined, options?: { disabledIsGone?: boolean }) => {
    const active = document.activeElement;
    wait.current =
      !focusLost() && active instanceof HTMLElement ? { from: active, find, thrown: false, disabledIsGone: options?.disabledIsGone } : null;
  }, []);
}

/// Whether a key press belongs to the composing of text by an input method (Chinese, Japanese,
/// Korean…): its Enter takes the composed text and its Escape drops it; neither may save, send
/// or close the field.
export function isComposing(event: { nativeEvent: { isComposing?: boolean }; keyCode: number }): boolean {
  return event.nativeEvent.isComposing === true || event.keyCode === 229;
}

/// How long a new row stays marked.
const FRESH_MS = 1400;

/// The ids of a list to mark as new: the ones not `known` before; none in the list's first read
/// (`known` is `null`), nor in a `quiet` one.
export function freshIds<T>(known: ReadonlySet<T> | null, ids: readonly T[], quiet: boolean): T[] {
  return known === null || quiet ? [] : arrived(known, ids);
}

/// The ids that came after the list was first shown (`ready`), for a moment: new rows can
/// slide in and light up. The ids of a `quiet` list are taken without a mark: a list read again
/// as it is shown brings what came while nobody watched (`freshIds`).
export function useFresh<T>(ids: readonly T[], ready: boolean, quiet = false): ReadonlySet<T> {
  const known = useRef<Set<T> | null>(null);
  const [fresh, setFresh] = useState<ReadonlySet<T>>(() => new Set());
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  useEffect(() => () => clearTimeout(timer.current), []);
  const signature = ids.join("\u0000");
  // Before the paint, so a new row never shows a frame without its mark.
  useLayoutEffect(() => {
    if (!ready) return;
    const added = freshIds(known.current, ids, quiet);
    if (known.current === null) known.current = new Set(ids);
    else for (const id of ids) known.current.add(id);
    if (added.length === 0) return;
    setFresh(new Set(added));
    clearTimeout(timer.current);
    timer.current = setTimeout(() => setFresh(new Set()), FRESH_MS);
    // `signature` stands for `ids`: a new array with the same ids is no change.
  }, [signature, ready, quiet]);
  return fresh;
}

/// How long a delete button lets presses go after it deleted: the row below moves up under the
/// pointer, and the second click of a double click must not delete it too. A button that asks
/// again lets them go as long after it asked (`confirmPress`).
export const DELETE_GUARD_MS = 400;

/// Presses that come too soon after `hold` are let go: the second click of a double click must
/// not act on what the first one has just put under the pointer.
export function pressGuard(now: () => number = Date.now) {
  let until = 0;
  const passes = () => now() >= until;
  const hold = (ms: number) => {
    until = Math.max(until, now() + ms);
  };
  return {
    /// Lets presses go for the next `ms` milliseconds.
    hold,
    /// Whether a press now acts.
    passes,
    /// Runs `action` for a press that passes, then lets presses go for `ms`.
    press(ms: number, action: () => void) {
      if (!passes()) return;
      hold(ms);
      action();
    },
  };
}

/// A `pressGuard` that lives as long as the component.
export function usePressGuard() {
  const guard = useRef<ReturnType<typeof pressGuard> | null>(null);
  if (guard.current === null) guard.current = pressGuard();
  return guard.current;
}

/// Calls `disarm` `after` (3 s by default) once `armed` became set: the armed row of a button that
/// asks again, so a much later single press never deletes, or what was copied (2 s).
function useDisarm(armed: unknown, disarm: () => void, after = 3000) {
  const latest = useRef(disarm);
  latest.current = disarm;
  useEffect(() => {
    if (armed === false || armed === null || armed === undefined) return;
    const timer = setTimeout(() => latest.current(), after);
    return () => clearTimeout(timer);
  }, [armed, after]);
}

/// What the copy buttons say: the key of what was copied ("Copied", for a moment) and the key of
/// what the clipboard refused (`last.copy_failed`, until the next copy).
export interface Copies<K> {
  copied: K | null;
  failed: K | null;
}

export const NO_COPIES: Copies<never> = { copied: null, failed: null };

/// The copy buttons after the copy of `key`, which the clipboard took or refused: only the latest
/// copy is spoken of.
export function afterCopy<K>(key: K, took: boolean): Copies<K> {
  return took ? { copied: key, failed: null } : { copied: null, failed: key };
}

/// Writes `text` to the clipboard, only on an explicit click, and says whether it could: the
/// clipboard may refuse, for one when the window has lost the focus.
export async function writeClipboard(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    return false;
  }
}

/// "Copied" for a moment after a copy button was pressed: `copied` is the key of what was
/// copied, for 2 s. `copy` writes the clipboard (`writeClipboard`) and says whether it could;
/// `failed` is then the key of what it refused, until the next copy. `reset` forgets both, for a
/// window that opens again.
export function useCopied<K>(): [K | null, (key: K, text: string) => Promise<boolean>, K | null, () => void] {
  const [copies, setCopies] = useState<Copies<K>>(NO_COPIES);
  useDisarm(copies.copied, () => setCopies((shown) => ({ ...shown, copied: null })), 2000);
  const copy = async (key: K, text: string) => {
    const took = await writeClipboard(text);
    setCopies(afterCopy(key, took));
    return took;
  };
  const reset = useCallback(() => setCopies(NO_COPIES), []);
  return [copies.copied, copy, copies.failed, reset];
}

/// The row armed by a button that asks again, and when (`Date.now()`).
export interface Armed<K> {
  key: K;
  at: number;
}

/// A press at `now` on a button that asks again, while `armed`: the armed key acts and disarms;
/// another key is armed instead. A press sooner than `DELETE_GUARD_MS` after the arming is the
/// second click of a double click, not a second thought: the key stays armed and nothing acts.
export function confirmPress<K>(armed: Armed<K> | null, key: K, now: number): { armed: Armed<K> | null; act: boolean } {
  if (armed === null || armed.key !== key) return { armed: { key, at: now }, act: false };
  if (now - armed.at < DELETE_GUARD_MS) return { armed, act: false };
  return { armed: null, act: true };
}

/// Destructive buttons that ask again, one armed at a time: the first press arms the row `key`,
/// the second within 3 s acts (`confirmPress`). Gives the armed key, the press and a disarm.
export function useConfirmKey<K>(): [K | null, (key: K, action: () => void) => void, () => void] {
  const [armed, setArmed] = useState<Armed<K> | null>(null);
  useDisarm(armed, () => setArmed(null));
  const press = (key: K, action: () => void) => {
    const next = confirmPress(armed, key, Date.now());
    setArmed(next.armed);
    if (next.act) action();
  };
  return [armed?.key ?? null, press, () => setArmed(null)];
}

/// A destructive button that asks again: the first press arms it, the second within 3 s acts.
export function useConfirm(): [boolean, (action: () => void) => void, () => void] {
  const [armed, press, disarm] = useConfirmKey<true>();
  return [armed !== null, (action) => press(true, action), disarm];
}

/// Whether a click on a dialog's backdrop closes it: the press must have begun there too, so a
/// selection dragged out of the dialog does not close it.
export function closesDialog(pressedBeside: boolean, target: EventTarget, backdrop: EventTarget): boolean {
  return pressedBeside && target === backdrop;
}

/// Where Tab takes the focus in a dialog, so it never leaves it: from the last of its `items`
/// to the first, back with Shift from the first to the last, and into the dialog from anywhere
/// else; `null` leaves the step to the browser.
export function trappedTab<T>(items: readonly T[], current: T | null, back: boolean): T | null {
  if (items.length === 0) return null;
  const at = current === null ? -1 : items.indexOf(current);
  const first = items[0];
  const last = items[items.length - 1];
  if (at < 0) return back ? last : first;
  if (back && at === 0) return last;
  if (!back && at === items.length - 1) return first;
  return null;
}

const FOCUSABLE = 'a[href], button:not(:disabled), input:not(:disabled), select:not(:disabled), textarea:not(:disabled), [tabindex]:not([tabindex="-1"])';

/// A dialog over the window, drawn at the end of the page so a hidden page cannot hide it; Escape
/// or a click beside it closes it. Escape is caught on the way down, so it closes only the
/// dialog: the quick panel hides on its own Escape listener. Tab stays in the dialog, and the
/// focus goes back where it was once the dialog closes. `inline` draws it where it stands: the
/// quick panel keeps the shade within its rounded edges.
export function Modal({
  labelledBy,
  onClose,
  inline = false,
  children,
}: {
  labelledBy: string;
  onClose: () => void;
  inline?: boolean;
  children: ReactNode;
}) {
  const pressedBeside = useRef(false);
  const close = useRef(onClose);
  close.current = onClose;
  const dialog = useRef<HTMLDivElement>(null);
  // Read while rendering: the dialog's own `autoFocus` takes the focus before its effects run.
  const [before] = useState(() => (document.activeElement instanceof HTMLElement ? document.activeElement : null));
  useEffect(() => {
    const box = dialog.current;
    const onKey = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") {
        event.stopPropagation();
        close.current();
        return;
      }
      if (event.key !== "Tab" || !box) return;
      const items = Array.from(box.querySelectorAll<HTMLElement>(FOCUSABLE));
      const focused = document.activeElement instanceof HTMLElement ? document.activeElement : null;
      const next = items.length === 0 ? box : trappedTab(items, focused, event.shiftKey);
      if (!next) return;
      event.preventDefault();
      next.focus();
    };
    window.addEventListener("keydown", onKey, true);
    if (box && !box.contains(document.activeElement)) box.focus();
    return () => {
      window.removeEventListener("keydown", onKey, true);
      // Only a dialog that has gone gives the focus back: in development React runs the effect
      // twice over the same dialog.
      if (!box?.isConnected) before?.focus();
    };
  }, [before]);
  const shade = (
    <div
      className="modal-backdrop"
      role="presentation"
      onMouseDown={(event) => {
        pressedBeside.current = event.target === event.currentTarget;
      }}
      onClick={(event) => {
        if (closesDialog(pressedBeside.current, event.target, event.currentTarget)) onClose();
      }}
    >
      <div ref={dialog} className="modal" role="dialog" aria-modal="true" aria-labelledby={labelledBy} tabIndex={-1}>
        {children}
      </div>
    </div>
  );
  return inline ? shade : createPortal(shade, document.body);
}

/// `indeterminate`: running without a measure of how far it is.
export function Progress({ value, label, indeterminate = false }: { value: number; label: string; indeterminate?: boolean }) {
  return (
    <span
      className={indeterminate ? "progress indeterminate" : "progress"}
      role="progressbar"
      aria-label={label}
      aria-valuemin={0}
      aria-valuemax={100}
      aria-valuenow={indeterminate ? undefined : Math.round(value * 100)}
    >
      <span style={indeterminate ? undefined : { width: `${Math.min(100, Math.max(0, value * 100))}%` }} />
    </span>
  );
}
