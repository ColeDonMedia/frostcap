import { m } from "motion/react";
import { useId, type ReactNode, type KeyboardEvent } from "react";
import { slideStyle } from "../lib/indicator";

export const spring = { type: "spring", stiffness: 520, damping: 34, mass: 0.7 } as const;
export const softSpring = { type: "spring", stiffness: 320, damping: 30 } as const;

// ------------------------------------------------------------------ Switch

export function Switch({
  checked,
  onChange,
  label,
  id,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  label: string;
  id?: string;
}) {
  return (
    <button
      id={id}
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      className="switch"
      onClick={() => onChange(!checked)}
    >
      <m.span className="switch__knob" animate={{ x: checked ? 18 : 0 }} transition={spring} />
    </button>
  );
}

// ------------------------------------------------------------------ Slider

export function Slider({
  label,
  value,
  min,
  max,
  step = 1,
  onChange,
  format,
  onCommit,
  disabled,
}: {
  label: string;
  value: number;
  min: number;
  max: number;
  step?: number;
  onChange: (v: number) => void;
  format?: (v: number) => string;
  onCommit?: () => void;
  disabled?: boolean;
}) {
  const id = useId();
  const pct = ((value - min) / (max - min)) * 100;
  const shown = format ? format(value) : String(value);
  return (
    <div className="field">
      <div className="field__row" style={{ minHeight: 20 }}>
        <label className="field__label" htmlFor={id}>
          {label}
        </label>
        <span className="field__value" aria-hidden="true">
          {shown}
        </span>
      </div>
      <input
        id={id}
        className="range"
        type="range"
        disabled={disabled}
        min={min}
        max={max}
        step={step}
        value={value}
        aria-valuetext={shown}
        style={{ ["--p" as string]: `${pct}%` }}
        onChange={(e) => onChange(Number(e.target.value))}
        onPointerUp={onCommit}
        onKeyUp={onCommit}
      />
    </div>
  );
}

// ------------------------------------------------------------- Segmented

export interface SegOption<T extends string> {
  value: T;
  label: ReactNode;
  icon?: ReactNode;
  ariaLabel?: string;
}

export function Segmented<T extends string>({
  value,
  options,
  onChange,
  label,
  disabled,
}: {
  value: T;
  options: SegOption<T>[];
  onChange: (v: T) => void;
  label: string;
  disabled?: boolean;
}) {
  const selIndex = options.findIndex((o) => o.value === value);
  const onKey = (e: KeyboardEvent<HTMLDivElement>) => {
    const i = options.findIndex((o) => o.value === value);
    let n = i;
    if (e.key === "ArrowRight" || e.key === "ArrowDown") n = (i + 1) % options.length;
    else if (e.key === "ArrowLeft" || e.key === "ArrowUp") n = (i - 1 + options.length) % options.length;
    else return;
    e.preventDefault();
    e.stopPropagation();
    onChange(options[n].value);
    const btn = e.currentTarget.querySelectorAll<HTMLButtonElement>("button")[n];
    btn?.focus();
  };
  return (
    <div className="seg" role="radiogroup" aria-label={label} aria-disabled={disabled || undefined} onKeyDown={disabled ? undefined : onKey}>
      <span className="seg__thumb" aria-hidden="true" style={slideStyle(selIndex, options.length)} />
      {options.map((o) => {
        const sel = o.value === value;
        return (
          <button
            key={o.value}
            type="button"
            role="radio"
            aria-checked={sel}
            aria-label={o.ariaLabel}
            tabIndex={sel ? 0 : -1}
            className="seg__btn"
            disabled={disabled}
            onClick={() => onChange(o.value)}
          >
            {o.icon && <span style={{ display: "inline-flex" }}>{o.icon}</span>}
            <span>{o.label}</span>
          </button>
        );
      })}
    </div>
  );
}

// ------------------------------------------------------------------ Select

export function Select<T extends string | number>({
  label,
  value,
  options,
  onChange,
  hideLabel,
  disabled,
}: {
  label: string;
  value: T;
  options: { value: T; label: string }[];
  onChange: (v: T) => void;
  hideLabel?: boolean;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <div className="field">
      <label className={hideLabel ? "sr-only" : "field__label"} htmlFor={id}>
        {label}
      </label>
      <select
        id={id}
        className="select"
        disabled={disabled}
        value={String(value)}
        onChange={(e) => {
          const raw = e.target.value;
          const opt = options.find((o) => String(o.value) === raw);
          if (opt) onChange(opt.value);
        }}
      >
        {options.map((o) => (
          <option key={String(o.value)} value={String(o.value)}>
            {o.label}
          </option>
        ))}
      </select>
    </div>
  );
}

// -------------------------------------------------------------- ToggleRow

export function ToggleRow({
  label,
  hint,
  checked,
  onChange,
  ariaLabel,
}: {
  label: string;
  hint?: string;
  /** Accessible name when the visible label is ambiguous (e.g. several "Mute" switches). */
  ariaLabel?: string;
  checked: boolean;
  onChange: (v: boolean) => void;
}) {
  const id = useId();
  return (
    <div className="field__row">
      <label htmlFor={id} className="field__label">
        {label}
        {hint && (
          <span className="field__hint" style={{ display: "block" }}>
            {hint}
          </span>
        )}
      </label>
      <Switch id={id} checked={checked} onChange={onChange} label={ariaLabel ?? label} />
    </div>
  );
}

export function Kbd({ children }: { children: ReactNode }) {
  return <kbd className="kbd">{children}</kbd>;
}
