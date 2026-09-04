import React from "react";
import { AlertTriangle } from "lucide-react";
import { Product } from "../../types/grocery";

interface LowStockAlertsProps {
  lowStockProducts: Product[];
  onAdjustStock: (barcode: string, delta: number) => void;
}

export const LowStockAlerts: React.FC<LowStockAlertsProps> = ({
  lowStockProducts,
  onAdjustStock,
}) => {
  if (lowStockProducts.length === 0) return null;

  return (
    <div
      className="mobile-card"
      style={{
        background: "rgba(245, 158, 11, 0.08)",
        border: "1px solid rgba(245, 158, 11, 0.28)",
      }}
    >
      <div className="card-title-row">
        <h3 className="card-heading" style={{ color: "#fbbf24" }}>
          <AlertTriangle size={18} />
          <span>Low-Stock Alerts ({lowStockProducts.length})</span>
        </h3>
        <span className="badge-mini badge-amber">Action Needed</span>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: "0.5rem" }}>
        {lowStockProducts.map((p) => (
          <div
            key={p.barcode}
            style={{
              background: "rgba(15, 23, 42, 0.8)",
              border: "1px solid rgba(245, 158, 11, 0.2)",
              borderRadius: "10px",
              padding: "0.65rem 0.85rem",
              display: "flex",
              justifyContent: "space-between",
              alignItems: "center",
            }}
          >
            <div>
              <div style={{ fontWeight: 600, fontSize: "0.88rem" }}>{p.name}</div>
              <div style={{ fontSize: "0.72rem", color: "#fbbf24" }}>
                Only {p.stock} units remaining
              </div>
            </div>
            <button
              id={`restock-${p.barcode}`}
              className="btn-mobile btn-mobile-admin"
              style={{ padding: "0.35rem 0.75rem", fontSize: "0.75rem" }}
              onClick={() => onAdjustStock(p.barcode, 10)}
            >
              +10 Restock
            </button>
          </div>
        ))}
      </div>
    </div>
  );
};
