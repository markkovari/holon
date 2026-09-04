import React from "react";
import { ShoppingBag, X, Minus, Plus, Trash2, CreditCard } from "lucide-react";
import { CartItem } from "../../types/grocery";

interface CartModalProps {
  isOpen: boolean;
  onClose: () => void;
  cart: CartItem[];
  onUpdateQty: (barcode: string, delta: number) => void;
  onRemove: (barcode: string) => void;
  cartTotalCents: number;
  onCheckout: () => void;
  loading: boolean;
}

export const CartModal: React.FC<CartModalProps> = ({
  isOpen,
  onClose,
  cart,
  onUpdateQty,
  onRemove,
  cartTotalCents,
  onCheckout,
  loading,
}) => {
  if (!isOpen) return null;

  return (
    <div className="mobile-modal-overlay" onClick={onClose}>
      <div className="mobile-bottom-sheet modal-content" onClick={(e) => e.stopPropagation()}>
        <div
          style={{
            display: "flex",
            justifyContent: "space-between",
            alignItems: "center",
            marginBottom: "1.2rem",
          }}
        >
          <div style={{ display: "flex", alignItems: "center", gap: "0.5rem" }}>
            <ShoppingBag size={20} style={{ color: "var(--accent)" }} />
            <h3 style={{ fontSize: "1.15rem", fontWeight: 700 }}>Your Basket</h3>
          </div>
          <button
            onClick={onClose}
            style={{
              background: "transparent",
              border: "none",
              color: "var(--text-muted)",
              cursor: "pointer",
            }}
          >
            <X size={20} />
          </button>
        </div>

        {cart.length === 0 ? (
          <div style={{ textAlign: "center", padding: "2rem 0", color: "var(--text-muted)" }}>
            <ShoppingBag size={40} style={{ opacity: 0.3, marginBottom: "0.75rem" }} />
            <p>Your basket is currently empty.</p>
            <p style={{ fontSize: "0.78rem" }}>Scan or tap items to add them.</p>
          </div>
        ) : (
          <div>
            <div
              style={{
                display: "flex",
                flexDirection: "column",
                gap: "0.6rem",
                maxHeight: "280px",
                overflowY: "auto",
                marginBottom: "1.2rem",
              }}
            >
              {cart.map((item) => (
                <div
                  key={item.product.barcode}
                  style={{
                    display: "flex",
                    justifyContent: "space-between",
                    alignItems: "center",
                    padding: "0.75rem",
                    background: "rgba(30, 41, 59, 0.45)",
                    borderRadius: "10px",
                    border: "1px solid var(--border)",
                  }}
                >
                  <div style={{ display: "flex", alignItems: "center", gap: "0.65rem" }}>
                    <span style={{ fontSize: "1.5rem" }}>{item.product.icon}</span>
                    <div>
                      <div style={{ fontWeight: 600, fontSize: "0.85rem" }}>
                        {item.product.name}
                      </div>
                      <div style={{ fontSize: "0.75rem", color: "var(--text-muted)" }}>
                        ${(item.product.price_cents / 100).toFixed(2)} each
                      </div>
                    </div>
                  </div>

                  <div style={{ display: "flex", alignItems: "center", gap: "0.6rem" }}>
                    <div style={{ display: "flex", alignItems: "center", gap: "0.2rem" }}>
                      <button
                        className="btn-mobile btn-mobile-outline"
                        style={{ padding: "2px 6px" }}
                        onClick={() => onUpdateQty(item.product.barcode, -1)}
                      >
                        <Minus size={12} />
                      </button>
                      <span
                        style={{
                          minWidth: "22px",
                          textAlign: "center",
                          fontWeight: 700,
                          fontSize: "0.85rem",
                        }}
                      >
                        {item.quantity}
                      </span>
                      <button
                        className="btn-mobile btn-mobile-outline"
                        style={{ padding: "2px 6px" }}
                        onClick={() => onUpdateQty(item.product.barcode, 1)}
                        disabled={item.quantity >= item.product.stock}
                      >
                        <Plus size={12} />
                      </button>
                    </div>
                    <button
                      id={`remove-item-${item.product.barcode}`}
                      onClick={() => onRemove(item.product.barcode)}
                      style={{
                        background: "transparent",
                        border: "none",
                        color: "#f87171",
                        cursor: "pointer",
                      }}
                    >
                      <Trash2 size={16} />
                    </button>
                  </div>
                </div>
              ))}
            </div>

            <div
              style={{
                borderTop: "1px solid var(--border)",
                paddingTop: "0.85rem",
                marginBottom: "1.25rem",
              }}
            >
              <div
                style={{
                  display: "flex",
                  justifyContent: "space-between",
                  marginBottom: "0.4rem",
                  color: "var(--text-secondary)",
                  fontSize: "0.82rem",
                }}
              >
                <span>Subtotal</span>
                <span>${(cartTotalCents / 100).toFixed(2)}</span>
              </div>
              <div
                style={{
                  display: "flex",
                  justifyContent: "space-between",
                  marginBottom: "0.4rem",
                  color: "var(--text-secondary)",
                  fontSize: "0.82rem",
                }}
              >
                <span>Estimated Tax (8%)</span>
                <span>${((cartTotalCents * 0.08) / 100).toFixed(2)}</span>
              </div>
              <div
                style={{
                  display: "flex",
                  justifyContent: "space-between",
                  fontSize: "1.1rem",
                  fontWeight: 800,
                }}
              >
                <span>Total Due</span>
                <span style={{ color: "#34d399" }}>
                  ${((cartTotalCents * 1.08) / 100).toFixed(2)}
                </span>
              </div>
            </div>

            <button
              id="checkout-btn"
              className="btn-mobile btn-mobile-primary"
              style={{ width: "100%", padding: "0.75rem", fontSize: "0.9rem" }}
              onClick={onCheckout}
              disabled={loading}
            >
              <CreditCard size={17} />
              <span>{loading ? "Processing..." : "Confirm & Pay Transaction"}</span>
            </button>
          </div>
        )}
      </div>
    </div>
  );
};
