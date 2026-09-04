import React from "react";
import { CheckCircle2 } from "lucide-react";

interface ToastProps {
  message: string | null;
}

export const Toast: React.FC<ToastProps> = ({ message }) => {
  if (!message) return null;

  return (
    <div className="toast-notice">
      <CheckCircle2 size={16} style={{ color: "var(--accent)" }} />
      <span>{message}</span>
    </div>
  );
};
