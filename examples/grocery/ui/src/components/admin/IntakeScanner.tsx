import React from "react";
import { Barcode, Upload } from "lucide-react";

interface IntakeScannerProps {
  scanning: boolean;
  onFileUpload: (e: React.ChangeEvent<HTMLInputElement>) => void;
  onTestFixture: (filename: string) => void;
}

export const IntakeScanner: React.FC<IntakeScannerProps> = ({
  scanning,
  onFileUpload,
  onTestFixture,
}) => {
  return (
    <div className="mobile-card">
      <div className="card-title-row">
        <h3 className="card-heading">
          <Barcode size={18} style={{ color: "var(--admin-accent)" }} />
          <span>Delivery Intake Scanner</span>
        </h3>
        <span className="badge-mini badge-purple">Admin Intake</span>
      </div>

      <label
        htmlFor="admin-scan-file-input"
        className="viewfinder"
        style={{ borderColor: "rgba(99, 102, 241, 0.3)" }}
      >
        <div
          className="viewfinder-laser"
          style={{
            background: "#818cf8",
            boxShadow: "0 0 10px #818cf8, 0 0 16px #6366f1",
          }}
        />
        <div className="corner corner-tl" style={{ borderColor: "#818cf8" }} />
        <div className="corner corner-tr" style={{ borderColor: "#818cf8" }} />
        <div className="corner corner-bl" style={{ borderColor: "#818cf8" }} />
        <div className="corner corner-br" style={{ borderColor: "#818cf8" }} />
        <Upload size={28} style={{ color: "var(--admin-accent)", marginBottom: "0.4rem" }} />
        <div style={{ fontWeight: 600, fontSize: "0.88rem", marginBottom: "0.2rem" }}>
          {scanning ? "Processing delivery barcode..." : "Scan Delivery Manifest PNG"}
        </div>
        <div style={{ fontSize: "0.72rem", color: "var(--text-muted)" }}>
          Pure compute WASI reader
        </div>
        <input
          id="admin-scan-file-input"
          type="file"
          accept="image/png"
          style={{ display: "none" }}
          onChange={onFileUpload}
        />
      </label>

      {/* Admin test fixtures */}
      <div style={{ marginTop: "0.75rem" }}>
        <div
          style={{
            fontSize: "0.7rem",
            color: "var(--text-muted)",
            fontWeight: 600,
            textTransform: "uppercase",
            letterSpacing: "0.04em",
            marginBottom: "0.35rem",
          }}
        >
          Delivery Manifest Fixtures:
        </div>
        <div className="chip-scroll">
          <button
            className="chip-btn"
            onClick={() => onTestFixture("code128-letters.png")}
          >
            Intake Code-128 (ZZG4ZDMEN)
          </button>
          <button
            className="chip-btn"
            onClick={() => onTestFixture("ean13-leading-zero.png")}
          >
            Intake UPC-A (0166131860910)
          </button>
        </div>
      </div>
    </div>
  );
};
