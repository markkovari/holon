import React from "react";
import { Package, Minus } from "lucide-react";
import { Product } from "../../types/grocery";

interface InventoryListProps {
  products: Product[];
  onAdjustStock: (barcode: string, delta: number) => void;
}

export const InventoryList: React.FC<InventoryListProps> = ({
  products,
  onAdjustStock,
}) => {
  return (
    <div className="mobile-card" id="admin-stock-table">
      <div className="card-title-row">
        <h3 className="card-heading">
          <Package size={18} style={{ color: "var(--admin-accent)" }} />
          <span>Real-Time Inventory</span>
        </h3>
        <span style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>
          {products.length} SKUs
        </span>
      </div>

      <div style={{ display: "flex", flexDirection: "column", gap: "0.6rem" }}>
        {products.map((p) => (
          <div key={p.barcode} className="product-row">
            <div className="product-info-group">
              <div
                className="product-emoji"
                style={{ background: "rgba(99, 102, 241, 0.12)" }}
              >
                {p.icon}
              </div>
              <div className="product-meta">
                <div className="product-name">{p.name}</div>
                <div className="product-sub">
                  <span
                    className="mono"
                    style={{ fontSize: "0.68rem", color: "#a5b4fc" }}
                  >
                    {p.barcode}
                  </span>
                  <span
                    className="badge-mini badge-purple"
                    style={{ fontSize: "0.6rem", padding: "1px 4px" }}
                  >
                    {p.symbology}
                  </span>
                </div>
              </div>
            </div>

            <div className="product-action">
              <span
                className={`badge-mini ${
                  p.stock <= 5 ? "badge-red" : "badge-green"
                }`}
              >
                {p.stock} units
              </span>
              <div
                style={{
                  display: "flex",
                  gap: "0.3rem",
                  marginTop: "0.25rem",
                }}
              >
                <button
                  id={`stock-dec-${p.barcode}`}
                  className="btn-mobile btn-mobile-outline"
                  style={{ padding: "2px 6px" }}
                  onClick={() => onAdjustStock(p.barcode, -1)}
                  disabled={p.stock <= 0}
                >
                  <Minus size={12} />
                </button>
                <button
                  id={`stock-inc-${p.barcode}`}
                  className="btn-mobile btn-mobile-outline"
                  style={{ padding: "2px 6px", fontWeight: 700 }}
                  onClick={() => onAdjustStock(p.barcode, 5)}
                >
                  +5
                </button>
              </div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
};
