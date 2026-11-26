import { useEffect, useState, type InputHTMLAttributes } from "react";

export function NumberFieldInput({
  value,
  onChange,
  allowEmpty = false,
  ...props
}: Omit<
  InputHTMLAttributes<HTMLInputElement>,
  "value" | "onChange" | "type"
> & {
  value: number | null | undefined;
  allowEmpty?: boolean;
  onChange: (next: number | null) => void;
}) {
  const [raw, setRaw] = useState(() =>
    typeof value === "number" && Number.isFinite(value) ? String(value) : "",
  );
  useEffect(() => {
    if (value == null) {
      if (allowEmpty) setRaw("");
      return;
    }
    if (!Number.isFinite(value)) return;
    setRaw((current) =>
      current.trim() !== "" && Number(current) === value
        ? current
        : String(value),
    );
  }, [value, allowEmpty]);
  return (
    <input
      {...props}
      type="number"
      value={raw}
      onChange={(e) => {
        const text = e.currentTarget.value;
        setRaw(text);
        if (e.currentTarget.validity?.badInput) {
          onChange(Number.NaN);
          return;
        }
        if (text.trim() === "") {
          onChange(allowEmpty ? null : Number.NaN);
          return;
        }
        const parsed = Number(text);
        onChange(Number.isFinite(parsed) ? parsed : Number.NaN);
      }}
      onBlur={(e) => {
        if (
          !allowEmpty &&
          (raw.trim() === "" || !Number.isFinite(Number(raw))) &&
          typeof value === "number" &&
          Number.isFinite(value)
        ) {
          setRaw(String(value));
        }
        props.onBlur?.(e);
      }}
    />
  );
}
