import React from "react";
import { Barcode, AlertTriangle, Plus } from "lucide-react";
import { Product, ScanResult } from "../../types/grocery";
import { ScannerViewfinder } from "../common/ScannerViewfinder";
import { ProductCatalog } from "./ProductCatalog";
import { FloatingCartBar } from "./FloatingCartBar";

interface ShopperViewProps {
  products: Product[];
  scanning: boolean;
  scanResult: ScanResult | null;
  scanError: string | null;
  onFileUpload: (e: React.ChangeEvent<HTMLInputElement>) => void;
  onTestFixture: (fixture: string) => void;
  onAddToCart: (product: Product) => void;
  cartTotalCents: number;
  cartItemCount: number;
  onOpenCart: () => void;
}

export const ShopperView: React.FC<ShopperViewProps> = ({
  products,
  scanning,
  scanResult,
  scanError,
  onFileUpload,
  onTestFixture,
  onAddToCart,
  cartTotalCents,
  cartItemCount,
  onOpenCart,
}) => {
  return (
    <div>
      <div className="shopper-layout">
        {/* Left Column: Live Scanner Pane */}
        <div className="scanner-sticky-pane">
          <div className="mobile-card">
            <div className="card-title-row">
              <h2 className="card-heading">
                <Barcode size={18} style={{ color: "var(--accent)" }} />
                <span>Laser Barcode Scanner</span>
              </h2>
              <span className="badge-mini badge-green">Pure WASI</span>
            </div>

            {/* Neon Viewfinder */}
            <ScannerViewfinder
              inputId="scan-file-input"
              scanning={scanning}
              onFileUpload={onFileUpload}
              title="Tap to Scan / Upload PNG"
              subtitle="Real-time decoding via POST /api/scan"
            />

            {/* Quick Fixture Chips */}
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
                Real Component Fixtures (Zero Mocking):
              </div>
              <div className="chip-scroll">
                <button
                  id="test-ean13-btn"
                  className="chip-btn"
                  onClick={() => onTestFixture("ean13.png")}
                  disabled={scanning}
                >
                  🫒 EAN-13 (Olive Oil)
                </button>
                <button
                  id="test-ean8-btn"
                  className="chip-btn"
                  onClick={() => onTestFixture("ean8.png")}
                  disabled={scanning}
                >
                  🥛 EAN-8 (Fresh Milk)
                </button>
                <button
                  id="test-upca-btn"
                  className="chip-btn"
                  onClick={() => onTestFixture("upca.png")}
                  disabled={scanning}
                >
                  🍞 UPC-A (Sourdough)
                </button>
                <button
                  id="test-code128-btn"
                  className="chip-btn"
                  onClick={() => onTestFixture("code128.png")}
                  disabled={scanning}
                >
                  🥑 Code-128 (Avocado)
                </button>
              </div>
            </div>

            {/* Scan Error Message */}
            {scanError && (
              <div
                id="scan-error-msg"
                style={{
                  marginTop: "0.75rem",
                  padding: "0.75rem",
                  borderRadius: "10px",
                  background: "rgba(239, 68, 68, 0.12)",
                  border: "1px solid rgba(239, 68, 68, 0.3)",
                  color: "#f87171",
                  fontSize: "0.8rem",
                  display: "flex",
                  alignItems: "center",
                  gap: "0.5rem",
                }}
              >
                <AlertTriangle size={16} />
                <span>{scanError}</span>
              </div>
            )}

            {/* Scan Success Card */}
            {scanResult && (
              <div
                id="scan-result-card"
                style={{
                  marginTop: "0.85rem",
                  padding: "0.9rem",
                  borderRadius: "12px",
                  background: "rgba(16, 185, 129, 0.08)",
                  border: "1px solid rgba(16, 185, 129, 0.25)",
                }}
              >
                <div
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                    marginBottom: "0.5rem",
                  }}
                >
                  <span className="badge-mini badge-green" id="scan-result-symbology">
                    {scanResult.barcode.symbology.toUpperCase()}
                  </span>
                  <span
                    className="mono"
                    id="scan-result-text"
                    style={{ fontSize: "0.85rem", fontWeight: 700, color: "#34d399" }}
                  >
                    {scanResult.barcode.text}
                  </span>
                </div>

                {scanResult.product ? (
                  <div
                    style={{
                      display: "flex",
                      alignItems: "center",
                      justifyContent: "space-between",
                      gap: "0.5rem",
                    }}
                  >
                    <div style={{ display: "flex", alignItems: "center", gap: "0.6rem" }}>
                      <span style={{ fontSize: "1.7rem" }}>{scanResult.product.icon}</span>
                      <div>
                        <div style={{ fontWeight: 600, fontSize: "0.9rem" }}>
                          {scanResult.product.name}
                        </div>
                        <div style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>
                          ${(scanResult.product.price_cents / 100).toFixed(2)} ·{" "}
                          {scanResult.product.stock} in stock
                        </div>
                      </div>
                    </div>
                    <button
                      id="add-scanned-to-cart-btn"
                      className="btn-mobile btn-mobile-primary"
                      onClick={() => onAddToCart(scanResult.product!)}
                    >
                      <Plus size={15} />
                      <span>Add</span>
                    </button>
                  </div>
                ) : (
                  <div style={{ fontSize: "0.8rem", color: "var(--text-secondary)" }}>
                    Barcode decoded by WASI. Item is not yet registered in catalog.
                  </div>
                )}
              </div>
            )}
          </div>
        </div>

        {/* Right Column: Catalog Browser Pane */}
        <div className="catalog-pane">
          <ProductCatalog products={products} onAddToCart={onAddToCart} />
        </div>
      </div>

      {/* Floating Bottom Bar (Mobile / Dock) */}
      <FloatingCartBar
        cartTotalCents={cartTotalCents}
        cartItemCount={cartItemCount}
        onOpenCart={onOpenCart}
      />
    </div>
  );
};
