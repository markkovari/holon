import React from "react";

interface KpiMetricsProps {
  productCount: number;
  lowStockCount: number;
  totalUnits: number;
}

export const KpiMetrics: React.FC<KpiMetricsProps> = ({
  productCount,
  lowStockCount,
  totalUnits,
}) => {
  return (
    <div
      className="admin-kpi-grid"
      style={{
        display: "grid",
        gridTemplateColumns: "1fr 1fr",
        gap: "0.65rem",
        marginBottom: "1.25rem",
      }}
    >
      <div className="mobile-card" style={{ margin: 0, padding: "0.85rem 1rem" }}>
        <div style={{ fontSize: "0.72rem", color: "var(--text-muted)", marginBottom: "0.2rem" }}>
          Catalog SKUs
        </div>
        <div style={{ fontSize: "1.35rem", fontWeight: 800, color: "#f8fafc" }}>
          {productCount}
        </div>
      </div>
      <div className="mobile-card" style={{ margin: 0, padding: "0.85rem 1rem" }}>
        <div style={{ fontSize: "0.72rem", color: "var(--text-muted)", marginBottom: "0.2rem" }}>
          Low Stock Alert
        </div>
        <div
          style={{
            fontSize: "1.35rem",
            fontWeight: 800,
            color: lowStockCount > 0 ? "#fbbf24" : "#34d399",
          }}
        >
          {lowStockCount} Items
        </div>
      </div>
      <div className="mobile-card" style={{ margin: 0, padding: "0.85rem 1rem" }}>
        <div style={{ fontSize: "0.72rem", color: "var(--text-muted)", marginBottom: "0.2rem" }}>
          Total Units
        </div>
        <div style={{ fontSize: "1.35rem", fontWeight: 800, color: "#34d399" }}>
          {totalUnits} Units
        </div>
      </div>
      <div className="mobile-card" style={{ margin: 0, padding: "0.85rem 1rem" }}>
        <div style={{ fontSize: "0.72rem", color: "var(--text-muted)", marginBottom: "0.2rem" }}>
          WASI Engine
        </div>
        <div style={{ fontSize: "0.95rem", fontWeight: 800, color: "#818cf8", paddingTop: "0.3rem" }}>
          Pure Compute
        </div>
      </div>
    </div>
  );
};
