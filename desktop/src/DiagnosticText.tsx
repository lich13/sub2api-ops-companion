import { useState } from "react";
import { Copy } from "lucide-react";
export default function DiagnosticText({ text }: { text: string }) {
  const [message, setMessage] = useState("");
  if (!text) return null;
  return (
    <div className="diagnostic-text">
      <span>{text}</span>
      <button
        type="button"
        aria-label="复制完整检测详情"
        title="复制完整检测详情"
        onClick={() =>
          void navigator.clipboard
            .writeText(text)
            .then(() => setMessage("已复制"))
            .catch(() => setMessage("复制失败，可选择文本复制"))
        }
      >
        <Copy size={13} />
      </button>
      {message && <small role="status">{message}</small>}
    </div>
  );
}
