import React from "react";
import { Upload } from "lucide-react";

interface ScannerViewfinderProps {
  inputId: string;
  scanning: boolean;
  onFileUpload: (e: React.ChangeEvent<HTMLInputElement>) => void;
  title?: string;
  subtitle?: string;
  laserStyle?: React.CSSProperties;
  cornerColor?: string;
  iconColor?: string;
  borderColor?: string;
}

export const ScannerViewfinder: React.FC<ScannerViewfinderProps> = ({
  inputId,
  scanning,
  onFileUpload,
  title = "Tap to Scan / Upload PNG",
  subtitle = "Real-time decoding via POST /api/scan",
  laserStyle,
  cornerColor = "#10b981",
  iconColor = "var(--accent)",
  borderColor,
}) => {
  return (
    <label
      htmlFor={inputId}
      className="viewfinder"
      id={inputId === "scan-file-input" ? "scanner-viewport" : undefined}
      style={borderColor ? { borderColor } : undefined}
    >
      <div className="viewfinder-laser" style={laserStyle} />
      <div className="corner corner-tl" style={cornerColor ? { borderColor: cornerColor } : undefined} />
      <div className="corner corner-tr" style={cornerColor ? { borderColor: cornerColor } : undefined} />
      <div className="corner corner-bl" style={cornerColor ? { borderColor: cornerColor } : undefined} />
      <div className="corner corner-br" style={cornerColor ? { borderColor: cornerColor } : undefined} />
      <Upload size={28} style={{ color: iconColor, marginBottom: "0.4rem" }} />
      <div style={{ fontWeight: 600, fontSize: "0.88rem", marginBottom: "0.2rem" }}>
        {scanning ? "Sweeping scanlines in WebAssembly..." : title}
      </div>
      <div style={{ fontSize: "0.72rem", color: "var(--text-muted)" }}>{subtitle}</div>
      <input
        id={inputId}
        type="file"
        accept="image/png"
        style={{ display: "none" }}
        onChange={onFileUpload}
        disabled={scanning}
      />
    </label>
  );
};
