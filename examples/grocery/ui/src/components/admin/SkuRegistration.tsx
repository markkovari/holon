import React from "react";
import { Package } from "lucide-react";

interface SkuRegistrationProps {
  newBarcode: string;
  newSymbology: string;
  newName: string;
  newCategory: string;
  newPrice: string;
  newStock: string;
  onBarcodeChange: (val: string) => void;
  onSymbologyChange: (val: string) => void;
  onNameChange: (val: string) => void;
  onCategoryChange: (val: string) => void;
  onPriceChange: (val: string) => void;
  onStockChange: (val: string) => void;
  onSubmit: (e: React.FormEvent) => void;
}

export const SkuRegistration: React.FC<SkuRegistrationProps> = ({
  newBarcode,
  newSymbology,
  newName,
  newCategory,
  newPrice,
  newStock,
  onBarcodeChange,
  onSymbologyChange,
  onNameChange,
  onCategoryChange,
  onPriceChange,
  onStockChange,
  onSubmit,
}) => {
  return (
    <div className="mobile-card">
      <div className="card-title-row">
        <h3 className="card-heading">
          <Package size={18} style={{ color: "var(--admin-accent)" }} />
          <span>Register / Update SKU</span>
        </h3>
      </div>

      <form onSubmit={onSubmit} style={{ display: "flex", flexDirection: "column", gap: "0.65rem" }}>
        <div style={{ display: "grid", gridTemplateColumns: "1.8fr 1fr", gap: "0.5rem" }}>
          <div>
            <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Barcode</label>
            <input
              id="new-sku-barcode"
              type="text"
              value={newBarcode}
              onChange={(e) => onBarcodeChange(e.target.value)}
              placeholder="e.g. 4006381333931"
              className="mobile-input mono"
            />
          </div>
          <div>
            <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Symbology</label>
            <select
              value={newSymbology}
              onChange={(e) => onSymbologyChange(e.target.value)}
              className="mobile-input"
            >
              <option value="ean-13">EAN-13</option>
              <option value="ean-8">EAN-8</option>
              <option value="upc-a">UPC-A</option>
              <option value="code-128">Code-128</option>
            </select>
          </div>
        </div>

        <div>
          <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Product Name</label>
          <input
            id="new-sku-name"
            type="text"
            value={newName}
            onChange={(e) => onNameChange(e.target.value)}
            placeholder="e.g. Premium Italian Roast"
            className="mobile-input"
          />
        </div>

        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr 1fr", gap: "0.5rem" }}>
          <div>
            <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Category</label>
            <select
              value={newCategory}
              onChange={(e) => onCategoryChange(e.target.value)}
              className="mobile-input"
            >
              <option value="Produce">Produce</option>
              <option value="Dairy">Dairy</option>
              <option value="Bakery">Bakery</option>
              <option value="Pantry">Pantry</option>
            </select>
          </div>
          <div>
            <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Price ($)</label>
            <input
              type="number"
              step="0.01"
              value={newPrice}
              onChange={(e) => onPriceChange(e.target.value)}
              className="mobile-input"
            />
          </div>
          <div>
            <label style={{ fontSize: "0.72rem", color: "var(--text-secondary)" }}>Stock</label>
            <input
              type="number"
              value={newStock}
              onChange={(e) => onStockChange(e.target.value)}
              className="mobile-input"
            />
          </div>
        </div>

        <button
          id="register-sku-btn"
          type="submit"
          className="btn-mobile btn-mobile-admin"
          style={{ marginTop: "0.4rem", padding: "0.65rem" }}
        >
          Save SKU to Catalog
        </button>
      </form>
    </div>
  );
};
